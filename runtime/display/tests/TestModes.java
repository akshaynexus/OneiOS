// Host test of the helper's choice among the panel's modes (OneiOSDisplay.pick): the native mode is the largest
// the panel has, at its highest rate, whatever mode One UI left active. Run by `make test`.
public final class TestModes {
    private static int checks, failures;

    private static void check(boolean ok, String what) {
        checks++;
        if (!ok) { failures++; System.out.println("FAIL " + what); }
    }

    public static void main(String[] args) {
        // the S24 Ultra's kind of list: three sizes, several rates each; One UI at FHD+ (id 5)
        int[] ids = {1, 2, 3, 4, 5, 6, 7}, w = {1440, 1440, 1440, 1080, 1080, 720, 1080}, h = {3120, 3120, 3120, 2340, 2340, 1560, 2340},
              hz = {60, 120, 24, 60, 120, 120, 24};
        int[] p = OneiOSDisplay.pick(ids, w, h, hz, 5);
        check(p != null && p[0] == 1080 && p[1] == 2340 && p[2] == 120, "the active mode and its highest rate");
        check(p != null && p[3] == 1440 && p[4] == 3120 && p[5] == 120 && p[6] == 2, "the native mode at 120 Hz, by id");
        p = OneiOSDisplay.pick(ids, w, h, hz, 3);
        check(p != null && p[0] == 1440 && p[1] == 3120 && p[2] == 120 && p[6] == 2, "active at 24 Hz: same size, the 120 Hz mode");
        check(OneiOSDisplay.pick(ids, w, h, hz, 9) == null, "no active mode");
        p = OneiOSDisplay.pick(new int[]{4}, new int[]{1080}, new int[]{2340}, new int[]{60}, 4);
        check(p != null && p[3] == 1080 && p[4] == 2340 && p[5] == 60 && p[6] == 4, "one mode is its own native");
        // the layer's buffers for the hello's formats, and --dataspace's names (src/xhd.h has the same values)
        check(OneiOSDisplay.bufferFormat(1, 5) == 5 && OneiOSDisplay.bufferFormat(1, 2) == 2, "8-bit frames: B,G,R,A, or R,G,B,X when asked");
        check(OneiOSDisplay.bufferFormat(2, 2) == 22 && OneiOSDisplay.bufferFormat(2, 5) == 22, "half-float frames: RGBA_FP16 whatever --format says");
        check(OneiOSDisplay.bufferFormat(3, 5) == 0, "an unknown frame format: no buffer");
        check(OneiOSDisplay.dataspace("auto") == -1 && OneiOSDisplay.dataspace("srgb") == 142671872 && OneiOSDisplay.dataspace("p3") == 143261696
              && OneiOSDisplay.dataspace("scrgb-linear") == 406913024 && OneiOSDisplay.dataspace("rec709") == -2, "dataspace names");
        check(OneiOSDisplay.colourModeName(9).equals("DISPLAY_P3") && OneiOSDisplay.colourModeName(0).equals("NATIVE")
              && OneiOSDisplay.colourModeName(42).equals("42"), "colour mode names");
        System.out.println("display mode tests: " + checks + " checks, " + failures + " failed");
        System.exit(failures == 0 ? 0 : 1);
    }
}
