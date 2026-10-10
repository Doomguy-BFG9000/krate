package tech.krate.player;

import android.app.Activity;
import android.content.Context;
import android.os.Build;
import android.os.VibrationAttributes;
import android.os.VibrationEffect;
import android.os.Vibrator;
import android.os.VibratorManager;
import android.util.Log;

/**
 * The phone tapping back (krate:ui/haptics, IC-917): each of Krate's seven
 * feels as the phone's own predefined effect where it has them (Android 10
 * and up), a short pulse shaped to match where it does not, and an impact's
 * strength as the pulse's amplitude. Played as touch feedback, so a person
 * who turned touch vibration off in Settings feels none, as with the phone's
 * own apps.
 */
final class Haptics {
    private Haptics() {}

    static final int LIGHT = 0;
    static final int MEDIUM = 1;
    static final int HEAVY = 2;
    static final int SELECTION = 3;
    static final int SUCCESS = 4;
    static final int WARNING = 5;
    static final int ERROR = 6;

    private static Vibrator vibrator;

    private static Vibrator vibrator(Activity activity) {
        if (vibrator == null) {
            if (Build.VERSION.SDK_INT >= 31) {
                VibratorManager manager =
                        (VibratorManager) activity.getSystemService(Context.VIBRATOR_MANAGER_SERVICE);
                vibrator = manager == null ? null : manager.getDefaultVibrator();
            } else {
                vibrator = (Vibrator) activity.getSystemService(Context.VIBRATOR_SERVICE);
            }
        }
        return vibrator;
    }

    static boolean available(Activity activity) {
        Vibrator v = vibrator(activity);
        return v != null && v.hasVibrator();
    }

    /** Play feel (one of the constants above), or an impact at strength when feel is -1. */
    static boolean play(Activity activity, int feel, float strength) {
        Vibrator v = vibrator(activity);
        if (v == null || !v.hasVibrator()) {
            return false;
        }
        VibrationEffect effect = effect(v, feel, strength);
        if (effect == null) {
            return true;
        }
        try {
            if (Build.VERSION.SDK_INT >= 33) {
                v.vibrate(effect, VibrationAttributes.createForUsage(VibrationAttributes.USAGE_TOUCH));
            } else {
                v.vibrate(effect);
            }
            return true;
        } catch (RuntimeException e) {
            Log.w(KrateActivity.TAG, "haptic " + feel + ": " + e);
            return false;
        }
    }

    private static VibrationEffect effect(Vibrator v, int feel, float strength) {
        boolean amplitude = v.hasAmplitudeControl();
        if (feel < 0) {
            if (!(strength > 0f)) {
                return null;
            }
            float s = Math.min(1f, strength);
            int level = Math.max(1, Math.round(255f * s));
            long ms = 12 + Math.round(28f * s);
            return VibrationEffect.createOneShot(ms, amplitude ? level : VibrationEffect.DEFAULT_AMPLITUDE);
        }
        if (Build.VERSION.SDK_INT >= 29) {
            switch (feel) {
                case LIGHT:
                case SELECTION:
                    return VibrationEffect.createPredefined(VibrationEffect.EFFECT_TICK);
                case MEDIUM:
                    return VibrationEffect.createPredefined(VibrationEffect.EFFECT_CLICK);
                case HEAVY:
                    return VibrationEffect.createPredefined(VibrationEffect.EFFECT_HEAVY_CLICK);
                case SUCCESS:
                    return VibrationEffect.createPredefined(VibrationEffect.EFFECT_DOUBLE_CLICK);
                default:
                    break;
            }
        }
        switch (feel) {
            case LIGHT:
                return pulse(amplitude, 10, 80);
            case SELECTION:
                return pulse(amplitude, 6, 60);
            case MEDIUM:
                return pulse(amplitude, 18, 150);
            case HEAVY:
                return pulse(amplitude, 30, 255);
            case SUCCESS:
                return pattern(amplitude, new long[] {0, 18, 70, 28}, new int[] {0, 140, 0, 220});
            case WARNING:
                return pattern(amplitude, new long[] {0, 30, 90, 30}, new int[] {0, 200, 0, 200});
            default:
                return pattern(amplitude, new long[] {0, 35, 60, 35, 60, 35},
                        new int[] {0, 255, 0, 255, 0, 255});
        }
    }

    private static VibrationEffect pulse(boolean amplitude, long ms, int level) {
        return VibrationEffect.createOneShot(ms, amplitude ? level : VibrationEffect.DEFAULT_AMPLITUDE);
    }

    private static VibrationEffect pattern(boolean amplitude, long[] timings, int[] levels) {
        if (!amplitude) {
            return VibrationEffect.createWaveform(timings, -1);
        }
        return VibrationEffect.createWaveform(timings, levels, -1);
    }
}
