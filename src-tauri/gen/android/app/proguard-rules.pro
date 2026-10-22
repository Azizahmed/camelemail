# Add project specific ProGuard rules here.
# You can control the set of applied configuration files using the
# proguardFiles setting in build.gradle.
#
# For more details, see
#   http://developer.android.com/guide/developing/tools/proguard.html

# If your project uses WebView with JS, uncomment the following
# and specify the fully qualified class name to the JavaScript interface
# class:
#-keepclassmembers class fqcn.of.javascript.interface.for.webview {
#   public *;
#}

# Uncomment this to preserve the line number information for
# debugging stack traces.
#-keepattributes SourceFile,LineNumberTable

# If you keep the line number information, uncomment this to
# hide the original source file name.
#-renamesourcefileattribute SourceFile
# Remail's own Tauri plugin is loaded by name (src-tauri/src/mobile.rs) and its
# command arguments are filled by reflection: keep them as they are.
-keep class app.remail.mobile.RemailPlugin { *; }
-keep class app.remail.mobile.FileArgs { *; }
-keep class app.remail.mobile.BarsArgs { *; }
