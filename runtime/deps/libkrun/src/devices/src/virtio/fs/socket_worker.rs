// socket_worker.rs - FUSE server over a byte stream (socket transport).
//
// This module provides a FUSE protocol server that communicates over a
// bidirectional byte stream (e.g., an AF_HYPERV/AF_VSOCK socket) instead of
// virtio queues. It reuses the existing Server<PassthroughFs> implementation
// by constructing buffer-backed Reader/Writer instances for each FUSE message.
//
// Architecture:
//   Host: SocketFuseWorker ←── socket ──→ Guest: fuse_mount
//         └─ Server<PassthroughFs>               └─ /dev/fuse bridge
//            (existing FUSE protocol handler)
//
// The FUSE protocol is self-framing:
//   Request:  [InHeader (40 bytes)][body]  — InHeader.len = total size
//   Response: [OutHeader (16 bytes)][body] — OutHeader.len = total size

use std::io::{self, Read, Write};
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::Arc;

use log::{error, info, warn};

use super::descriptor_utils::{Reader, Writer};
use super::passthrough::{self, PassthroughFs};
use super::server::Server;

/// Size of the FUSE InHeader struct.
const IN_HEADER_SIZE: usize = 40;

/// Maximum FUSE message size (1 MB data + 4 KB header overhead).
const MAX_MSG_SIZE: usize = (1 << 20) + 0x1000;

/// Response buffer size — must fit the largest possible FUSE response.
const RESPONSE_BUF_SIZE: usize = (1 << 20) + 0x1000;


/// Run a FUSE protocol server on an already-connected bidirectional stream.
///
/// This function takes ownership of `stream` (any type implementing `Read + Write + Send`)
/// and serves FUSE protocol messages using `Server<PassthroughFs>` backed by the given
/// root directory.
///
/// The function blocks until the stream is closed, an error occurs, or the `stop` flag
/// is set.
///
/// # Arguments
///
/// * `name` - Human-readable label for logging.
/// * `root_dir` - Host directory path to expose via FUSE passthrough.
/// * `stream` - Connected bidirectional byte stream (e.g., a socket).
/// * `stop` - Atomic flag checked between messages; set to `true` to stop.
pub fn serve_fuse_on_stream<S: Read + Write>(
    name: &str,
    root_dir: &str,
    mut stream: S,
    stop: &Arc<AtomicBool>,
) {
    // Create the passthrough filesystem.
    let mut fs_config = passthrough::Config::default();
    fs_config.root_dir = root_dir.to_string();
    fs_config.export_table = None;
    fs_config.writeback = false;
    #[cfg(target_os = "windows")]
    {
        fs_config.announce_submounts = false;
    }
    fs_config.export_fsid = 0;
    fs_config.allow_root_dir_delete = false;
    let fs = match PassthroughFs::new(fs_config) {
        Ok(fs) => fs,
        Err(e) => {
            error!("fuse_server[{}]: failed to create PassthroughFs: {}", name, e);
            return;
        }
    };
    let server = Server::new(fs);
    let exit_code = Arc::new(AtomicI32::new(0));

    let mut req_buf = vec![0u8; MAX_MSG_SIZE];
    let mut resp_buf = vec![0u8; RESPONSE_BUF_SIZE];
    let mut msg_count: u64 = 0;

    info!("fuse_server[{}]: serving FUSE protocol for {}", name, root_dir);

    loop {
        if stop.load(Ordering::Relaxed) {
            info!("fuse_server[{}]: stop signal received", name);
            break;
        }

        // 1. Read the InHeader (40 bytes).
        if let Err(e) = stream.read_exact(&mut req_buf[..IN_HEADER_SIZE]) {
            if e.kind() == io::ErrorKind::UnexpectedEof {
                info!("fuse_server[{}]: client disconnected", name);
            } else {
                warn!("fuse_server[{}]: read header error: {}", name, e);
            }
            break;
        }

        // 2. Parse message length from InHeader.len (first 4 bytes, little-endian).
        let msg_len =
            u32::from_le_bytes([req_buf[0], req_buf[1], req_buf[2], req_buf[3]]) as usize;
        let opcode = u32::from_le_bytes([req_buf[4], req_buf[5], req_buf[6], req_buf[7]]);
        let unique = u64::from_le_bytes([
            req_buf[8], req_buf[9], req_buf[10], req_buf[11], req_buf[12], req_buf[13],
            req_buf[14], req_buf[15],
        ]);

        if msg_len < IN_HEADER_SIZE || msg_len > MAX_MSG_SIZE {
            error!(
                "fuse_server[{}]: invalid message length: {} (min={}, max={})",
                name, msg_len, IN_HEADER_SIZE, MAX_MSG_SIZE
            );
            break;
        }

        // 3. Read the rest of the request body.
        let remaining = msg_len - IN_HEADER_SIZE;
        if remaining > 0 {
            if let Err(e) = stream.read_exact(&mut req_buf[IN_HEADER_SIZE..msg_len]) {
                warn!("fuse_server[{}]: read body error: {}", name, e);
                break;
            }
        }

        // 4. Create buffer-backed Reader and Writer.
        let reader = Reader::from_buffer(&req_buf[..msg_len]);
        let writer = Writer::from_buffer(&mut resp_buf[..]);

        // 5. Dispatch through the existing FUSE server.
        match server.handle_message(
            reader,
            writer,
            &None, // No VirtioShmRegion (DAX not supported over socket)
            &exit_code,
            #[cfg(target_os = "macos")]
            &None,
        ) {
            Ok(resp_len) => {
                msg_count += 1;

                // No-reply operations (FORGET, BATCH_FORGET, INTERRUPT,
                // DESTROY) return resp_len == 0.  Do NOT send anything back
                // – the guest bridge must not wait for a response either.
                if resp_len == 0 {
                    if msg_count <= 8 {
                        debug!(
                            "fuse_server[{}]: msg#{} opcode={} unique={} -> no-reply (resp_len=0)",
                            name, msg_count, opcode, unique
                        );
                    }
                    continue;
                }

                if msg_count <= 2 {
                    info!(
                        "fuse_server[{}]: msg#{} op={} unique={} -> resp_len={}",
                        name, msg_count, opcode, unique, resp_len
                    );
                }
                // 6. Send the response back.
                if let Err(e) = stream.write_all(&resp_buf[..resp_len]) {
                    warn!("fuse_server[{}]: write response error: {}", name, e);
                    break;
                }
            }
            Err(e) => {
                error!(
                    "fuse_server[{}]: handle_message error for opcode={} unique={}: {:?}",
                    name,
                    opcode,
                    unique,
                    e
                );
                // Send an EIO error response so the client doesn't hang.
                let error_resp = make_error_response(&req_buf[..msg_len]);
                if let Err(e) = stream.write_all(&error_resp) {
                    warn!("fuse_server[{}]: write error response failed: {}", name, e);
                    break;
                }
            }
        }
    }

    info!("fuse_server[{}]: exiting", name);
}

/// Build a minimal FUSE error response (EIO) from a request buffer.
fn make_error_response(req: &[u8]) -> Vec<u8> {
    const OUT_HEADER_SIZE: usize = 16;
    const LINUX_EIO: i32 = 5;

    let unique = if req.len() >= IN_HEADER_SIZE {
        // unique is at offset 8 in InHeader (after len:u32 + opcode:u32)
        u64::from_le_bytes([
            req[8], req[9], req[10], req[11], req[12], req[13], req[14], req[15],
        ])
    } else {
        0
    };

    let mut resp = vec![0u8; OUT_HEADER_SIZE];
    resp[0..4].copy_from_slice(&(OUT_HEADER_SIZE as u32).to_le_bytes());
    resp[4..8].copy_from_slice(&(-LINUX_EIO).to_le_bytes());
    resp[8..16].copy_from_slice(&unique.to_le_bytes());
    resp
}
