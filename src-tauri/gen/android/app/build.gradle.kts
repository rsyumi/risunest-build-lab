import java.util.Properties
import java.net.URI
import java.io.ByteArrayOutputStream
import org.jetbrains.kotlin.gradle.tasks.KotlinCompile

plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.android")
    id("rust")
}

val tauriProperties = Properties().apply {
    val propFile = file("tauri.properties")
    if (propFile.exists()) {
        propFile.inputStream().use { load(it) }
    }
}
val controlDevOrigin = providers.gradleProperty("risunestControlDevOrigin")
    .orElse(providers.environmentVariable("TAURI_DEV_HOST").map { "http://$it:5174" })
    .orElse("http://localhost:5174")
    .map { origin ->
        val uri = URI(origin)
        require(uri.scheme in listOf("http", "https") && uri.host != null &&
            uri.userInfo == null && uri.rawPath.isNullOrEmpty() &&
            uri.rawQuery == null && uri.rawFragment == null && uri.port in -1..65535) {
            "risunestControlDevOrigin must be one exact HTTP(S) origin"
        }
        "\"$origin\""
    }
val enableExperimentalSafFileJobs = providers
    .gradleProperty("risuEnableExperimentalSafFileJobs")
    .map { it.equals("true", ignoreCase = true) }
    .orElse(true)

// Release signing. `keystore.properties` lives next to this module (in
// src-tauri/gen/android) and is gitignored, so a checkout without it still
// builds; the release artifact is then simply unsigned. Never fall back to the
// debug key: an APK signed with the debug key looks installable but can never
// be upgraded by a properly signed build.
val keystorePropertiesFile = rootProject.file("keystore.properties")
val keystoreProperties = Properties().apply {
    if (keystorePropertiesFile.exists()) {
        keystorePropertiesFile.inputStream().use { load(it) }
    }
}
val releaseStoreFile = keystoreProperties.getProperty("storeFile")
    ?.takeIf { it.isNotBlank() }
    ?.let { file(it) }
val releaseSigningReady = keystorePropertiesFile.exists() &&
    releaseStoreFile != null &&
    releaseStoreFile.exists() &&
    !keystoreProperties.getProperty("storePassword").isNullOrBlank() &&
    !keystoreProperties.getProperty("keyAlias").isNullOrBlank() &&
    !keystoreProperties.getProperty("keyPassword").isNullOrBlank()

android {
    compileSdk = 36
    ndkVersion = "28.2.13676358"
    namespace = "io.github.rsyumi.risunest"
    defaultConfig {
        applicationId = "io.github.rsyumi.risunest"
        minSdk = 24
        targetSdk = 36
        versionCode = tauriProperties.getProperty("tauri.android.versionCode", "1").toInt()
        versionName = tauriProperties.getProperty("tauri.android.versionName", "1.0")
        buildConfigField("String", "CONTROL_DEV_ORIGIN", "\"\"")
        buildConfigField(
            "boolean",
            "ENABLE_EXPERIMENTAL_SAF_FILE_JOBS",
            enableExperimentalSafFileJobs.get().toString(),
        )
    }
    signingConfigs {
        if (releaseSigningReady) {
            create("release") {
                storeFile = releaseStoreFile
                storePassword = keystoreProperties.getProperty("storePassword")
                keyAlias = keystoreProperties.getProperty("keyAlias")
                keyPassword = keystoreProperties.getProperty("keyPassword")
            }
        }
    }
    buildTypes {
        getByName("debug") {
            isDebuggable = true
            isJniDebuggable = true
            buildConfigField("String", "CONTROL_DEV_ORIGIN", controlDevOrigin.get())
            isMinifyEnabled = false
            // The Tauri template kept the full DWARF debug info of the Rust
            // cdylib in the APK. The dev-profile librisunest_lib.so is ~470 MB,
            // of which ~285 MB is debug info, and `useLegacyPackaging = false`
            // stores it uncompressed, so the debug APK was ~476 MB. Letting AGP
            // strip the packaged copy leaves the unstripped library in
            // src-tauri/target/<triple>/debug/ for ndk-stack symbolication.
        }
        getByName("release") {
            isMinifyEnabled = true
            if (releaseSigningReady) {
                signingConfig = signingConfigs.getByName("release")
            }
            proguardFiles(
                *fileTree(".") { include("**/*.pro") }
                    .plus(getDefaultProguardFile("proguard-android-optimize.txt"))
                    .toList().toTypedArray()
            )
        }
    }
    kotlinOptions {
        jvmTarget = "1.8"
    }
    buildFeatures {
        buildConfig = true
    }
    packaging {
        jniLibs {
            useLegacyPackaging = false
        }
    }
}

rust {
    rootDirRel = "../../../"
}

// Native TLS verifies servers through Android with the Kotlin half of
// rustls-platform-verifier, published to that project's Maven archive. Its
// version must equal rustls-platform-verifier-android in the Cargo lock.
val rustlsPlatformVerifierVersion = providers.exec {
    workingDir = rootProject.file("../..")
    commandLine(
        "cargo", "metadata", "--format-version", "1", "--locked",
        "--filter-platform", "aarch64-linux-android", "--manifest-path", "Cargo.toml",
    )
}.standardOutput.asText.map { metadata ->
    @Suppress("UNCHECKED_CAST")
    val packages = (groovy.json.JsonSlurper().parseText(metadata) as Map<String, Any?>)["packages"]
        as List<Map<String, Any?>>
    packages.single { it["name"] == "rustls-platform-verifier-android" }["version"] as String
}

repositories {
    maven {
        url = uri("https://github.com/rustls/rustls-platform-verifier/raw/maven-archive/android-release-support/maven/")
        content { includeGroup("org.rustls") }
    }
}

dependencies {
    implementation("org.rustls:rustls-platform-verifier:${rustlsPlatformVerifierVersion.get()}")
    implementation("org.jetbrains.kotlinx:kotlinx-coroutines-android:1.7.3")
    implementation("androidx.webkit:webkit:1.14.0")
    implementation("androidx.appcompat:appcompat:1.7.1")
    implementation("androidx.activity:activity-ktx:1.10.1")
    implementation("com.google.android.material:material:1.12.0")
    implementation("androidx.lifecycle:lifecycle-process:2.10.0")
    testImplementation("junit:junit:4.13.2")
}

apply(from = "tauri.build.gradle.kts")

val prepareWryRendererRecovery = tasks.register<PrepareWryRendererRecovery>("prepareWryRendererRecovery") {
    generatedDirectory = file("src/main/java/io/github/rsyumi/risunest/generated")
    mustRunAfter(tasks.withType<BuildTask>())
}
tasks.withType<KotlinCompile>().configureEach {
    dependsOn(prepareWryRendererRecovery)
}

afterEvaluate {
    if (!releaseSigningReady) {
        tasks.matching {
            (it.name.startsWith("assemble") || it.name.startsWith("bundle")) &&
                it.name.endsWith("Release")
        }.configureEach {
            doLast {
                logger.warn(
                    "RisuNest: release signing is not configured (${keystorePropertiesFile.path} " +
                        "missing or incomplete), so this release artifact is UNSIGNED and cannot be " +
                        "installed. See docs/research/android-build-audit-2026-09-06.md.",
                )
            }
        }
    }
    val universalDebugUnitTest = tasks.named(
        "testUniversalDebugUnitTest",
        org.gradle.api.tasks.testing.Test::class.java,
    )
    tasks.withType(org.gradle.api.tasks.testing.Test::class.java).configureEach {
        if (name.endsWith("UnitTest")) {
            filter {
                excludeTestsMatching("io.github.rsyumi.risunest.SafFileBridgeLowMemoryTest")
            }
        }
    }
    tasks.register(
        "testLowMemorySafCopy",
        org.gradle.api.tasks.testing.Test::class.java,
    ) {
        group = "verification"
        description = "Runs the SAF stream copy test with a 32 MiB JVM heap."
        testClassesDirs = universalDebugUnitTest.get().testClassesDirs
        classpath = universalDebugUnitTest.get().classpath
        minHeapSize = "16m"
        maxHeapSize = "32m"
        filter {
            includeTestsMatching("io.github.rsyumi.risunest.SafFileBridgeLowMemoryTest")
        }
    }
}


tasks.matching { it.name.startsWith("minify") && it.name.endsWith("ReleaseWithR8") }.configureEach {
    doLast {
        val variant = name.removePrefix("minify").removeSuffix("WithR8").replaceFirstChar { it.lowercase() }
        val mapping = layout.buildDirectory.file("outputs/mapping/$variant/mapping.txt").get().asFile
        check(mapping.isFile) { "Missing R8 mapping for JNI retention verification: $variant" }
        val lines = mapping.readLines()
        val dexDumpName = if (System.getProperty("os.name").startsWith("Windows")) "dexdump.exe" else "dexdump"
        val dexDumpTool = android.sdkDirectory.resolve("build-tools/${android.buildToolsVersion}/$dexDumpName")
        check(dexDumpTool.isFile) { "Missing SDK dexdump for JNI retention verification: $dexDumpTool" }
        val dexFiles = outputs.files.asFileTree.matching { include("**/*.dex") }.files
        check(dexFiles.isNotEmpty()) { "Missing optimized DEX for JNI retention verification: $variant" }
        val nativeInitializers = mutableSetOf<String>()
        for (dex in dexFiles) {
            val dump = ByteArrayOutputStream()
            project.exec {
                executable(dexDumpTool)
                args(dex)
                standardOutput = dump
            }.assertNormalExitValue()
            nativeInitializers += retainedJniNativeInitializers(dump.toString(Charsets.UTF_8.name()))
        }
        for (owner in listOf("ExternalStorageSecrets", "ServerSyncSecrets")) {
            val className = "io.github.rsyumi.risunest.$owner"
            val classIndex = lines.indexOf("$className -> $className:")
            check(classIndex >= 0) { "R8 renamed or removed JNI class: $className" }
            val members = lines.drop(classIndex + 1).takeWhile { it.startsWith(" ") || it.startsWith("#") }
            for (method in listOf("seal", "open", "removeKeys")) {
                check(members.any { it.contains(" $method(") && it.endsWith(" -> $method") }) {
                    "R8 renamed or removed JNI method: $className.$method"
                }
            }
            check(owner in nativeInitializers) {
                "R8 renamed or removed static native JNI method: $className.initialize()V"
            }
        }
        check("PlatformTls" in nativeInitializers) {
            "R8 renamed or removed static native JNI method: " +
                "io.github.rsyumi.risunest.PlatformTls.initialize(Landroid/content/Context;)V"
        }
    }
}
