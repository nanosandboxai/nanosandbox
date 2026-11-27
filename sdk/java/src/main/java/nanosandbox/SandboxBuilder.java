package nanosandbox;

/**
 * A fluent builder for a sandbox creation spec. {@link #build()} returns the
 * spec object that {@link SandboxClient#createWith(SandboxBuilder)} sends as
 * the {@code create} request's {@code spec} field.
 */
public final class SandboxBuilder {

    private final Json spec = Json.object();

    /** Starts a builder for a sandbox with the given image. */
    public SandboxBuilder(String image) {
        spec.set("image", Json.ofString(image));
    }

    /** Sets the sandbox name. */
    public SandboxBuilder withName(String name) {
        spec.set("name", Json.ofString(name));
        return this;
    }

    /** Sets the CPU/memory resource limits. */
    public SandboxBuilder withResources(int cpus, int memoryMb) {
        spec.set("cpus", Json.ofNumber(cpus));
        spec.set("memory_mb", Json.ofNumber(memoryMb));
        return this;
    }

    /** Adds an environment variable for the guest. */
    public SandboxBuilder withEnv(String key, String value) {
        Json env = spec.get("env");
        if (env.kind() != Json.Kind.OBJECT) {
            env = Json.object();
            spec.set("env", env);
        }
        env.set(key, Json.ofString(value));
        return this;
    }

    /** Publishes a TCP port from the host to the container. */
    public SandboxBuilder publishTcp(int hostPort, int containerPort) {
        Json net = spec.get("network");
        if (net.kind() != Json.Kind.OBJECT) {
            net = Json.object();
            spec.set("network", net);
        }
        net.set("enabled", Json.ofBool(true));
        Json pub = net.get("publish");
        if (pub.kind() != Json.Kind.ARRAY) {
            pub = Json.array();
            net.set("publish", pub);
        }
        Json entry = Json.object();
        entry.set("host_port", Json.ofNumber(hostPort));
        entry.set("container_port", Json.ofNumber(containerPort));
        entry.set("protocol", Json.ofString("tcp"));
        pub.add(entry);
        return this;
    }

    /** Adds a label to the sandbox. */
    public SandboxBuilder withLabel(String key, String value) {
        Json labels = spec.get("labels");
        if (labels.kind() != Json.Kind.OBJECT) {
            labels = Json.object();
            spec.set("labels", labels);
        }
        labels.set(key, Json.ofString(value));
        return this;
    }

    /** Marks the sandbox as ephemeral. */
    public SandboxBuilder ephemeral(boolean ephemeral) {
        spec.set("ephemeral", Json.ofBool(ephemeral));
        return this;
    }

    /** Sets the sandbox hostname. */
    public SandboxBuilder withHostname(String hostname) {
        spec.set("hostname", Json.ofString(hostname));
        return this;
    }

    /** Returns the built spec object. */
    public Json build() {
        return spec;
    }
}