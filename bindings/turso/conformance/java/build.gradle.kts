// Names the driver for `fylo link`; run.sh runs Conformance.java directly
// against the Maven Central jar.
plugins { java }
repositories { mavenCentral() }
dependencies { implementation("tech.turso:turso:0.8.3") }
