# The JNI functions are found by name: keep FyloNative and its native methods,
# and the FyloKeystore functions that FYLO's native code calls.
-keep class io.github.d31ma.fylo.FyloNative { native <methods>; }
-keep class io.github.d31ma.fylo.FyloKeystore { public static byte[] *(...); }
