package tech.krate.player;

import android.app.KeyguardManager;
import android.content.Context;
import android.hardware.biometrics.BiometricManager;
import android.hardware.biometrics.BiometricPrompt;
import android.os.Build;
import android.os.CancellationSignal;
import android.util.Log;

/**
 * Asking the person to prove it is them (krate:ui/unlock, IC-924): the
 * system's own BiometricPrompt, a fingerprint or face with the screen lock
 * (PIN, pattern, password) as the other way in. The app never sees a finger,
 * a face or a PIN; it hears yes, no, or that the phone cannot check.
 */
final class Unlock {
    private Unlock() {}

    static final int ASKING = -1;
    static final int YES = 0;
    static final int NO = 1;
    static final int CANNOT = 2;

    private static int answer = ASKING;
    private static String why = "";

    private static int authenticators() {
        return BiometricManager.Authenticators.BIOMETRIC_STRONG
                | BiometricManager.Authenticators.DEVICE_CREDENTIAL;
    }

    /** Whether this phone can ask at all: a screen lock is set. */
    static boolean available(Context context) {
        if (Build.VERSION.SDK_INT >= 30) {
            BiometricManager manager = context.getSystemService(BiometricManager.class);
            return manager != null
                    && manager.canAuthenticate(authenticators()) == BiometricManager.BIOMETRIC_SUCCESS;
        }
        KeyguardManager lock = (KeyguardManager) context.getSystemService(Context.KEYGUARD_SERVICE);
        return Build.VERSION.SDK_INT >= 28 && lock != null && lock.isDeviceSecure();
    }

    /** Show the system's sheet with reason; the answer comes from answer(). */
    @SuppressWarnings("deprecation")
    static void check(final KrateActivity activity, final String reason, final String app) {
        synchronized (Unlock.class) {
            answer = ASKING;
            why = "";
        }
        if (!available(activity)) {
            finish(CANNOT, "This phone has no screen lock: set one in Settings.");
            return;
        }
        activity.runOnUiThread(new Runnable() {
            @Override
            public void run() {
                BiometricPrompt.Builder builder = new BiometricPrompt.Builder(activity)
                        .setTitle(reason)
                        .setSubtitle(app + " is asking, through Krate");
                if (Build.VERSION.SDK_INT >= 30) {
                    builder.setAllowedAuthenticators(authenticators());
                } else {
                    builder.setDeviceCredentialAllowed(true);
                }
                BiometricPrompt prompt = builder.build();
                prompt.authenticate(new CancellationSignal(), activity.getMainExecutor(),
                        new BiometricPrompt.AuthenticationCallback() {
                            @Override
                            public void onAuthenticationSucceeded(
                                    BiometricPrompt.AuthenticationResult result) {
                                finish(YES, "");
                            }

                            @Override
                            public void onAuthenticationError(int code, CharSequence text) {
                                switch (code) {
                                    case BiometricPrompt.BIOMETRIC_ERROR_USER_CANCELED:
                                    case BiometricPrompt.BIOMETRIC_ERROR_CANCELED:
                                    case BiometricPrompt.BIOMETRIC_ERROR_LOCKOUT:
                                    case BiometricPrompt.BIOMETRIC_ERROR_LOCKOUT_PERMANENT:
                                    case BiometricPrompt.BIOMETRIC_ERROR_TIMEOUT:
                                    case 13: // BIOMETRIC_ERROR_NEGATIVE_BUTTON
                                        finish(NO, String.valueOf(text));
                                        break;
                                    default:
                                        finish(CANNOT, String.valueOf(text));
                                        break;
                                }
                            }
                        });
                Log.i(KrateActivity.TAG, "unlock: asking");
            }
        });
    }

    private static void finish(int value, String text) {
        synchronized (Unlock.class) {
            answer = value;
            why = text;
        }
        Log.i(KrateActivity.TAG, "unlock: " + (value == YES ? "yes" : value == NO ? "no"
                : "cannot (" + text + ")"));
    }

    static synchronized int answer() {
        return answer;
    }

    static synchronized String why() {
        return why;
    }
}
