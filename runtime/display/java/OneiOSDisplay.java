// SPDX-License-Identifier: MIT
// The display helper's Java half: a root, framework-independent SurfaceControl layer on the built-in
// display (the pattern of the OneOS fork's OneOSBootStatus, which draws while system_server is down).
// Run by app_process through bin/oneios-display; only surfaceflinger has to be running.
// Every android.* class is reached by reflection, so this compiles with a JDK alone and a method a
// release renamed is a fallback here, not a build error. The socket, the mapped frame and the copy
// into the layer's buffers are native (src/jni.c).

import java.lang.reflect.Method;

public final class OneiOSDisplay {
    private static native long nOpen(String socket, int waitMs);
    private static native int[] nInfo(long handle);                    // width, height, stride, format
    private static native int[] nFit(int sw, int sh, int dw, int dh);  // x, y, w, h
    private static native int nRun(long handle, Object surface, int format);
    private static native void nClose(long handle);

    private static final int RGBX_8888 = 2, BGRA_8888 = 5;
    private static final int TOP = Integer.MAX_VALUE - 1;
    private static Class<?> SC, TX;

    private static void log(String line) { System.err.println("oneios-display: " + line); }

    public static void main(String[] args) {
        int code;
        try { code = run(args); } catch (Throwable failure) { failure.printStackTrace(System.err); code = 5; }
        System.err.flush();
        System.exit(code);
    }

    private static int usage() {
        log("usage: oneios-display (--session DIR | --socket PATH) [--takeover] [--wait SECONDS] [--logical WxH]"
            + " [--hz N] [--format bgra|rgbx] [--no-projection]");
        return 2;
    }

    private static int run(String[] args) throws Exception {
        String socket = null, lib = null;
        int wait = 30, hz = 0, format = BGRA_8888;
        boolean takeover = false, projection = true;
        int[] logical = null;
        for (int i = 0; i < args.length; i++) {
            String a = args[i];
            if (a.equals("--takeover")) { takeover = true; continue; }
            if (a.equals("--no-projection")) { projection = false; continue; }
            if (i + 1 >= args.length) return usage();
            String v = args[++i];
            if (a.equals("--session")) socket = v + "/display.sock";
            else if (a.equals("--socket")) socket = v;
            else if (a.equals("--lib")) lib = v;
            else if (a.equals("--wait")) wait = Integer.parseInt(v);
            else if (a.equals("--hz")) hz = Integer.parseInt(v);
            else if (a.equals("--logical")) { String[] p = v.split("x"); logical = new int[]{Integer.parseInt(p[0]), Integer.parseInt(p[1])}; }
            else if (a.equals("--format")) format = v.equals("rgbx") ? RGBX_8888 : BGRA_8888;
            else return usage();
        }
        if (socket == null || lib == null) return usage();
        System.load(lib);
        SC = Class.forName("android.view.SurfaceControl");
        TX = Class.forName("android.view.SurfaceControl$Transaction");

        // The frame first: without xnuhost's socket there is nothing to show and no layer is made.
        long handle = nOpen(socket, wait * 1000);
        if (handle == 0) return 3;
        int[] guest = nInfo(handle);
        log("guest frame " + guest[0] + "x" + guest[1] + ", stride " + guest[2]);

        Object transaction = TX.getConstructor().newInstance();
        int[] panel = panel(transaction, takeover && projection, takeover, logical);
        if (hz <= 0) hz = panel[2];
        int[] fit = nFit(guest[0], guest[1], panel[0], panel[1]);
        log("panel " + panel[0] + "x" + panel[1] + " at " + hz + " Hz; the frame at " + fit[0] + "," + fit[1] + " " + fit[2] + "x" + fit[3]);

        Object back = null, backQueue = null, backSurface = null, layer = null, queue = null, surface = null;
        int result;
        try {
            // Black behind the frame where its aspect leaves bars (and under its first buffer).
            if (fit[2] != panel[0] || fit[3] != panel[1]) {
                back = layer("OneiOS display background");
                backQueue = queue("OneiOS display background", back, panel[0], panel[1], RGBX_8888);
                backSurface = backQueue.getClass().getMethod("createSurface").invoke(backQueue);
                place(transaction, back, TOP - 1);
                tx(transaction, "apply", new Class<?>[]{});
                Class<?> rect = Class.forName("android.graphics.Rect");
                Object canvas = backSurface.getClass().getMethod("lockCanvas", rect).invoke(backSurface, (Object) null);
                canvas.getClass().getMethod("drawColor", int.class).invoke(canvas, 0xff000000);
                backSurface.getClass().getMethod("unlockCanvasAndPost", Class.forName("android.graphics.Canvas")).invoke(backSurface, canvas);
            }
            // The frame's layer: buffers of the guest's size, scaled to the fit by the compositor
            // (one sampling on the GPU or in an overlay; this process only copies rows).
            layer = layer("OneiOS display");
            queue = queue("OneiOS display", layer, guest[0], guest[1], format);
            surface = queue.getClass().getMethod("createSurface").invoke(queue);
            float sx = fit[2] / (float) guest[0], sy = fit[3] / (float) guest[1];
            Class<?>[] f4 = {SC, float.class, float.class, float.class, float.class};
            if (!tx(transaction, "setMatrix", f4, layer, sx, 0f, 0f, sy)
                && !tx(transaction, "setScale", new Class<?>[]{SC, float.class, float.class}, layer, sx, sy))
                log("the layer could not be scaled: the frame shows at its own size");
            if (!tx(transaction, "setPosition", new Class<?>[]{SC, float.class, float.class}, layer, (float) fit[0], (float) fit[1]))
                log("the layer could not be placed");
            tx(transaction, "setOpaque", new Class<?>[]{SC, boolean.class}, layer, true);   // the guest's X byte is not alpha
            if (!tx(transaction, "setFrameRate", new Class<?>[]{SC, float.class, int.class}, layer, (float) hz, 0))
                log("no frame rate vote: surfaceflinger keeps its own refresh rate");
            place(transaction, layer, TOP);
            tx(transaction, "apply", new Class<?>[]{});
            log("layer up");
            result = nRun(handle, surface, format);
        } finally {
            for (Object gone : new Object[]{layer, back}) {
                if (gone == null) continue;
                if (!tx(transaction, "remove", new Class<?>[]{SC}, gone)) tx(transaction, "hide", new Class<?>[]{SC}, gone);
            }
            tx(transaction, "apply", new Class<?>[]{});
            for (Object s : new Object[]{surface, backSurface}) if (s != null) quiet(s, "release");
            for (Object q : new Object[]{queue, backQueue}) if (q != null) quiet(q, "destroy");
            nClose(handle);
        }
        log("layer removed");
        return result < 0 ? 4 : 0;
    }

    // ---- layers -------------------------------------------------------------------------------

    private static Object layer(String name) throws Exception {
        Object builder = Class.forName("android.view.SurfaceControl$Builder").getConstructor().newInstance();
        builder.getClass().getMethod("setName", String.class).invoke(builder, name);
        builder.getClass().getMethod("setBLASTLayer").invoke(builder);
        return builder.getClass().getMethod("build").invoke(builder);
    }

    private static Object queue(String name, Object layer, int width, int height, int format) throws Exception {
        Class<?> blast = Class.forName("android.graphics.BLASTBufferQueue");
        try {
            return blast.getConstructor(String.class, SC, int.class, int.class, int.class).newInstance(name, layer, width, height, format);
        } catch (NoSuchMethodException changed) {
            Object queue = blast.getConstructor(String.class, boolean.class).newInstance(name, false);
            blast.getMethod("update", SC, int.class, int.class, int.class).invoke(queue, layer, width, height, format);
            return queue;
        }
    }

    /** Top of the built-in display's stack, and visible. */
    private static void place(Object transaction, Object layer, int z) throws Exception {
        if (!tx(transaction, "setLayer", new Class<?>[]{SC, int.class}, layer, z)) throw new IllegalStateException("setLayer is gone");
        if (!tx(transaction, "setLayerStack", new Class<?>[]{SC, int.class}, layer, 0)) log("no setLayerStack: the layer stays on the default stack");
        if (!tx(transaction, "show", new Class<?>[]{SC}, layer)
            && !tx(transaction, "setVisibility", new Class<?>[]{SC, boolean.class}, layer, true))
            throw new IllegalStateException("the layer cannot be shown");
    }

    private static boolean tx(Object transaction, String name, Class<?>[] types, Object... args) {
        try { TX.getMethod(name, types).invoke(transaction, args); return true; }
        catch (Throwable missing) { return false; }
    }

    private static void quiet(Object target, String method) {
        try { target.getClass().getMethod(method).invoke(target); } catch (Throwable ignored) { }
    }

    // ---- the panel ----------------------------------------------------------------------------

    /**
     * Width and height of the space layers are placed in, and the refresh rate to ask for.
     * In a takeover session (system_server gone, surfaceflinger restarted at the return) the display's
     * projection is set to the active mode, unrotated: the layer space is then the panel's own pixels,
     * whatever resolution setting or rotation One UI left behind. Otherwise nothing of the display is
     * changed: the logical size comes from the display manager (One UI running), --logical, or the mode.
     */
    private static int[] panel(Object transaction, boolean project, boolean takeover, int[] logical) {
        Object token = null;
        int[] mode = null;                 // width, height, hz
        try {
            Object[] display = display();
            token = display[1];
            mode = mode((Long) display[0], token);
        } catch (Throwable unavailable) {
            log("the display's modes are not readable (" + unavailable + ")");
        }
        int hz = mode != null && mode[2] > 0 ? mode[2] : 120;
        if (project && token != null && mode != null) {
            try {
                Class<?> rect = Class.forName("android.graphics.Rect");
                Object full = rect.getConstructor(int.class, int.class, int.class, int.class).newInstance(0, 0, mode[0], mode[1]);
                TX.getMethod("setDisplayProjection", Class.forName("android.os.IBinder"), int.class, rect, rect)
                    .invoke(transaction, token, 0, full, full);
                TX.getMethod("apply").invoke(transaction);
                log("display projection set to the active mode " + mode[0] + "x" + mode[1]);
                return new int[]{mode[0], mode[1], hz};
            } catch (Throwable refused) {
                log("the display projection could not be set (" + refused + ")");
            }
        }
        if (!takeover) {
            try {
                Class<?> dm = Class.forName("android.hardware.display.DisplayManagerGlobal");
                Object instance = dm.getMethod("getInstance").invoke(null);
                Object info = dm.getMethod("getDisplayInfo", int.class).invoke(instance, 0);
                return new int[]{info.getClass().getField("logicalWidth").getInt(info), info.getClass().getField("logicalHeight").getInt(info), hz};
            } catch (Throwable noFramework) { /* system_server is not there: the next source */ }
        }
        if (logical != null) return new int[]{logical[0], logical[1], hz};
        if (mode != null) return new int[]{mode[0], mode[1], hz};
        log("panel size unknown: assuming 1440x3120");
        return new int[]{1440, 3120, hz};
    }

    /** The built-in display: {Long id, IBinder token}. */
    private static Object[] display() throws Exception {
        try {
            // Android 14 and later: the calls moved to DisplayControl in services.jar (scrcpy's way in).
            String jars = System.getenv("SYSTEMSERVERCLASSPATH");
            if (jars == null || jars.isEmpty()) jars = "/system/framework/services.jar";
            ClassLoader loader = (ClassLoader) Class.forName("dalvik.system.PathClassLoader")
                .getConstructor(String.class, ClassLoader.class).newInstance(jars, ClassLoader.getSystemClassLoader());
            Class<?> control = loader.loadClass("com.android.server.display.DisplayControl");
            Method load = Runtime.class.getDeclaredMethod("loadLibrary0", Class.class, String.class);
            load.setAccessible(true);
            load.invoke(Runtime.getRuntime(), control, "android_servers");
            long[] ids = (long[]) control.getMethod("getPhysicalDisplayIds").invoke(null);
            return new Object[]{ids[0], control.getMethod("getPhysicalDisplayToken", long.class).invoke(null, ids[0])};
        } catch (Throwable older) {
            long[] ids = (long[]) SC.getMethod("getPhysicalDisplayIds").invoke(null);
            return new Object[]{ids[0], SC.getMethod("getPhysicalDisplayToken", long.class).invoke(null, ids[0])};
        }
    }

    /** The active mode's width and height, and the highest refresh rate among the modes of that size. */
    private static int[] mode(long id, Object token) throws Exception {
        Object info;
        try { info = SC.getMethod("getDynamicDisplayInfo", long.class).invoke(null, id); }
        catch (NoSuchMethodException older) { info = SC.getMethod("getDynamicDisplayInfo", Class.forName("android.os.IBinder")).invoke(null, token); }
        int active = info.getClass().getField("activeDisplayModeId").getInt(info);
        Object[] modes = (Object[]) info.getClass().getField("supportedDisplayModes").get(info);
        int width = 0, height = 0;
        for (Object m : modes) if (m.getClass().getField("id").getInt(m) == active) {
            width = m.getClass().getField("width").getInt(m);
            height = m.getClass().getField("height").getInt(m);
        }
        if (width <= 0 || height <= 0) throw new IllegalStateException("no active mode");
        float best = 0;
        for (Object m : modes) {
            if (m.getClass().getField("width").getInt(m) != width || m.getClass().getField("height").getInt(m) != height) continue;
            float rate;
            try { rate = m.getClass().getField("peakRefreshRate").getFloat(m); }
            catch (NoSuchFieldException older) { rate = m.getClass().getField("refreshRate").getFloat(m); }
            if (rate > best) best = rate;
        }
        return new int[]{width, height, Math.round(best)};
    }
}
