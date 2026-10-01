/*
 * LabelProbe — prints "{package}\t{label}\t{uid}\t{system}" for every installed app.
 *
 * Why this exists: an application's display name ("酷安", "设置") is not available
 * from any adb command. `dumpsys package`, `pm dump`, `cmd package dump` and
 * `dumpsys activity recents` all expose at most `labelRes`, a resource *id* — the
 * text itself lives in the APK's `resources.arsc`, and parsing that on the host
 * means pulling megabytes per app.
 *
 * The device can answer the question directly: `PackageManager.getApplicationLabel()`
 * resolves the label, in the device's own language, for every app in one call. This
 * class is that call, executed through `app_process` — no root, no install, ~10 KB
 * pushed to /data/local/tmp and deleted afterwards.
 *
 * Deliberately written against **reflection only**: no `android.*` import means no
 * `android.jar` is needed to compile, no API level is baked into the dex, and the
 * output cannot break when a symbol moves between Android versions. It is the same
 * reason the dex is architecture-independent: one artefact covers arm64, arm32 and
 * x86 devices.
 *
 * Compile:  javac --release 8 -d classes LabelProbe.java
 *           d8 --min-api 24 --no-desugaring --output . classes/LabelProbe.class
 * Usage:    adb push classes.dex /data/local/tmp/labelprobe.dex
 *           adb shell CLASSPATH=/data/local/tmp/labelprobe.dex app_process /system/bin LabelProbe
 */

import java.lang.reflect.Field;
import java.lang.reflect.Method;
import java.util.ArrayList;
import java.util.Collections;
import java.util.Comparator;
import java.util.List;

public final class LabelProbe {

    /** `ApplicationInfo.FLAG_SYSTEM` — a system app rather than a user install. */
    private static final int FLAG_SYSTEM = 0x00000001;

    public static void main(String[] args) {
        try {
            run();
        } catch (Throwable failure) {
            // Print to stderr so a caller can tell "no apps" from "probe failed".
            System.err.println("LabelProbe failed: " + failure);
            failure.printStackTrace(System.err);
            System.exit(1);
        }
    }

    private static void run() throws Exception {
        // A Looper has to exist before `systemMain()`: it builds Handlers, and a
        // Handler throws "Can't create handler inside thread Thread[main,5,main]
        // that has not called Looper.prepare()" without one. This was the actual
        // failure on Android 17 (InvocationTargetException wrapping that
        // RuntimeException), and it is the one step every app_process tool does
        // first.
        Class<?> looper = Class.forName("android.os.Looper");
        try {
            looper.getMethod("prepareMainLooper").invoke(null);
        } catch (Throwable ignored) {
            // Already prepared by the runtime, or refused because one exists: both
            // mean the precondition is satisfied.
        }

        // ActivityThread.systemMain() gives the system context without needing an
        // Activity; it is how every `app_process` tool reaches the framework.
        Class<?> activityThread = Class.forName("android.app.ActivityThread");
        Object thread = activityThread.getMethod("systemMain").invoke(null);
        Object context = activityThread.getMethod("getSystemContext").invoke(thread);
        Object packageManager = context.getClass().getMethod("getPackageManager").invoke(context);

        Class<?> pmClass = Class.forName("android.content.pm.PackageManager");
        Class<?> aiClass = Class.forName("android.content.pm.ApplicationInfo");

        Method getInstalledApplications = pmClass.getMethod("getInstalledApplications", int.class);
        Method getApplicationLabel = pmClass.getMethod("getApplicationLabel", aiClass);
        Field packageName = aiClass.getField("packageName");
        Field uid = aiClass.getField("uid");
        Field flags = aiClass.getField("flags");

        @SuppressWarnings("unchecked")
        List<Object> apps = (List<Object>) getInstalledApplications.invoke(packageManager, Integer.valueOf(0));

        List<String> lines = new ArrayList<String>(apps == null ? 0 : apps.size());
        if (apps != null) {
            for (Object app : apps) {
                String name = String.valueOf(packageName.get(app));
                String label;
                try {
                    Object resolved = getApplicationLabel.invoke(packageManager, app);
                    label = resolved == null ? "" : String.valueOf(resolved);
                } catch (Throwable ignored) {
                    // A package with no resolvable label still has to appear: the
                    // caller falls back to the package name.
                    label = "";
                }
                int appUid = uid.getInt(app);
                boolean system = (flags.getInt(app) & FLAG_SYSTEM) != 0;
                // Tab-separated, with tabs and newlines stripped from the label so a
                // multi-line or padded label cannot break the row format.
                lines.add(name + "\t" + clean(label) + "\t" + appUid + "\t" + (system ? "1" : "0"));
            }
        }

        Collections.sort(lines, new Comparator<String>() {
            public int compare(String left, String right) {
                return left.compareTo(right);
            }
        });

        StringBuilder output = new StringBuilder(lines.size() * 48);
        for (int i = 0; i < lines.size(); i++) {
            output.append(lines.get(i)).append('\n');
        }
        System.out.print(output.toString());
        System.out.flush();
    }

    /** Collapses whitespace, so one app is always exactly one output line. */
    private static String clean(String value) {
        StringBuilder out = new StringBuilder(value.length());
        boolean lastWasSpace = false;
        for (int i = 0; i < value.length(); i++) {
            char c = value.charAt(i);
            if (c == '\n' || c == '\r' || c == '\t' || c == ' ') {
                if (!lastWasSpace && out.length() > 0) {
                    out.append(' ');
                    lastWasSpace = true;
                }
            } else {
                out.append(c);
                lastWasSpace = false;
            }
        }
        int end = out.length();
        while (end > 0 && out.charAt(end - 1) == ' ') {
            end--;
        }
        out.setLength(end);
        return out.toString();
    }
}
