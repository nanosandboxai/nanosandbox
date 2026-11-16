plugins {
    `java-library`
    kotlin("jvm") version "2.2.20"
    `maven-publish`
}

group = "ai.nanosandbox"
version = "0.2.0"

repositories {
    mavenCentral()
}

java {
    toolchain {
        languageVersion.set(JavaLanguageVersion.of(21))
    }
    withSourcesJar()
}

sourceSets {
    create("javaSdk") {
        java.srcDirs("../java/src/main/java", "src/nanosandbox")
        java.include("**/*.java")
        kotlin.setSrcDirs(emptyList<String>())
        resources.setSrcDirs(emptyList<String>())
    }
    named("main") {
        kotlin.srcDirs("src/nanosandbox")
        compileClasspath += sourceSets["javaSdk"].output
        runtimeClasspath += sourceSets["javaSdk"].output
    }
    create("tools") {
        kotlin.srcDirs("conformance", "e2e", "src/test")
        compileClasspath += sourceSets["main"].output + sourceSets["javaSdk"].output + configurations["runtimeClasspath"]
        runtimeClasspath += output + compileClasspath
    }
}

dependencies {
    implementation(kotlin("stdlib"))
}

tasks.named<Jar>("jar") {
    from(sourceSets["javaSdk"].output)
}

tasks.register<JavaExec>("smokeTest") {
    group = "verification"
    description = "Runs the Kotlin facade smoke test against an in-process fake helper."
    mainClass.set("nanosandbox.kt.SmokeTestKt")
    classpath = sourceSets["tools"].runtimeClasspath
}

tasks.register<JavaExec>("conformance") {
    group = "verification"
    description = "Runs the 35 protocol conformance scenarios via the Kotlin driver."
    mainClass.set("nanosandbox.kt.RunKt")
    classpath = sourceSets["tools"].runtimeClasspath
    workingDir = layout.projectDirectory.dir("../..").asFile
    val harness = System.getenv("NANOSB_CONFORMANCE_BIN")
        ?: layout.projectDirectory.dir("../../target/debug").file("nanosb-conformance").asFile.absolutePath
    environment("NANOSB_CONFORMANCE_BIN", harness)
}

publishing {
    publications {
        create<MavenPublication>("maven") {
            artifactId = "nanosandbox-sdk-kotlin"
            from(components["java"])
            pom {
                name.set("nanosandbox-sdk-kotlin")
                description.set("Kotlin facade over the nanosandbox Java SDK (nanosb-protocol v1.0.0)")
                url.set("https://github.com/nanosandboxai/nanosandbox")
                licenses {
                    license {
                        name.set("Apache-2.0")
                        url.set("https://www.apache.org/licenses/LICENSE-2.0.txt")
                    }
                }
                developers {
                    developer {
                        name.set("DevDone Labs")
                        organization.set("nanosandboxai")
                    }
                }
                scm {
                    connection.set("scm:git:https://github.com/nanosandboxai/nanosandbox.git")
                    url.set("https://github.com/nanosandboxai/nanosandbox")
                }
            }
        }
    }
    repositories {
        maven {
            name = "local-repo"
            val target = providers.gradleProperty("mavenRepoUrl").orNull
                ?: layout.buildDirectory.dir("maven-repo").get().asFile.absolutePath
            url = uri(target)
        }
    }
}
