package tech.krate.player;

import android.app.Activity;
import android.content.Context;
import android.hardware.Sensor;
import android.hardware.SensorEvent;
import android.hardware.SensorEventListener;
import android.hardware.SensorManager;
import android.view.Surface;

/**
 * How the phone is held (krate:ui/motion, IC-917): gravity, the push given
 * the phone, its turning and the compass, from SensorManager, in the
 * phone's own frame (x right, y up, z out of the glass). Rust turns them
 * into the screen's frame with the display's rotation, which read() hands
 * back too. Started on the first read, stopped when the app closes.
 */
final class Motion implements SensorEventListener {
    private static Motion running;

    private final SensorManager sensors;
    private final float[] gravity = new float[3];
    private final float[] linear = new float[3];
    private final float[] turning = new float[3];
    private final float[] rotationVector = new float[4];
    private boolean haveGravity;
    private boolean haveLinear;
    private boolean haveRotation;
    /** No gravity sensor: gravity is the accelerometer, smoothed. */
    private boolean fromAccelerometer;

    private Motion(SensorManager sensors) {
        this.sensors = sensors;
    }

    private static SensorManager manager(Activity activity) {
        return (SensorManager) activity.getSystemService(Context.SENSOR_SERVICE);
    }

    static boolean available(Activity activity) {
        SensorManager sm = manager(activity);
        return sm != null && (sm.getDefaultSensor(Sensor.TYPE_GRAVITY) != null
                || sm.getDefaultSensor(Sensor.TYPE_ACCELEROMETER) != null);
    }

    /**
     * The latest reading: gravity, linear acceleration (m/s^2), turning
     * (rad/s), the compass azimuth in degrees or -1, and the display's
     * rotation (0..3); null before the first sample or without sensors.
     */
    static synchronized float[] read(Activity activity) {
        if (running == null) {
            SensorManager sm = manager(activity);
            if (sm == null) {
                return null;
            }
            Motion motion = new Motion(sm);
            Sensor gravity = sm.getDefaultSensor(Sensor.TYPE_GRAVITY);
            Sensor accel = sm.getDefaultSensor(Sensor.TYPE_ACCELEROMETER);
            if (gravity == null && accel == null) {
                return null;
            }
            int rate = SensorManager.SENSOR_DELAY_GAME;
            if (gravity != null) {
                sm.registerListener(motion, gravity, rate);
                Sensor linear = sm.getDefaultSensor(Sensor.TYPE_LINEAR_ACCELERATION);
                if (linear != null) {
                    sm.registerListener(motion, linear, rate);
                }
            } else {
                motion.fromAccelerometer = true;
                sm.registerListener(motion, accel, rate);
            }
            Sensor gyro = sm.getDefaultSensor(Sensor.TYPE_GYROSCOPE);
            if (gyro != null) {
                sm.registerListener(motion, gyro, rate);
            }
            Sensor rotation = sm.getDefaultSensor(Sensor.TYPE_ROTATION_VECTOR);
            if (rotation != null) {
                sm.registerListener(motion, rotation, rate);
            }
            running = motion;
        }
        return running.snapshot(displayRotation(activity));
    }

    @SuppressWarnings("deprecation")
    private static int displayRotation(Activity activity) {
        switch (activity.getWindowManager().getDefaultDisplay().getRotation()) {
            case Surface.ROTATION_90:
                return 1;
            case Surface.ROTATION_180:
                return 2;
            case Surface.ROTATION_270:
                return 3;
            default:
                return 0;
        }
    }

    static synchronized void stop() {
        if (running != null) {
            running.sensors.unregisterListener(running);
            running = null;
        }
    }

    private synchronized float[] snapshot(int rotation) {
        if (!haveGravity) {
            return null;
        }
        float heading = -1f;
        if (haveRotation) {
            float[] matrix = new float[9];
            float[] angles = new float[3];
            SensorManager.getRotationMatrixFromVector(matrix, rotationVector);
            SensorManager.getOrientation(matrix, angles);
            heading = (float) Math.toDegrees(angles[0]);
            if (heading < 0f) {
                heading += 360f;
            }
        }
        float[] out = new float[11];
        System.arraycopy(gravity, 0, out, 0, 3);
        if (haveLinear) {
            System.arraycopy(linear, 0, out, 3, 3);
        }
        System.arraycopy(turning, 0, out, 6, 3);
        out[9] = heading;
        out[10] = rotation;
        return out;
    }

    @Override
    public synchronized void onSensorChanged(SensorEvent event) {
        float[] v = event.values;
        switch (event.sensor.getType()) {
            case Sensor.TYPE_GRAVITY:
                System.arraycopy(v, 0, gravity, 0, 3);
                haveGravity = true;
                break;
            case Sensor.TYPE_LINEAR_ACCELERATION:
                System.arraycopy(v, 0, linear, 0, 3);
                haveLinear = true;
                break;
            case Sensor.TYPE_ACCELEROMETER:
                if (fromAccelerometer) {
                    // A low-pass filter keeps gravity; the rest is the push.
                    float keep = haveGravity ? 0.9f : 0f;
                    for (int i = 0; i < 3; i++) {
                        gravity[i] = keep * gravity[i] + (1f - keep) * v[i];
                        linear[i] = v[i] - gravity[i];
                    }
                    haveGravity = true;
                    haveLinear = true;
                }
                break;
            case Sensor.TYPE_GYROSCOPE:
                System.arraycopy(v, 0, turning, 0, 3);
                break;
            case Sensor.TYPE_ROTATION_VECTOR:
                java.util.Arrays.fill(rotationVector, 0f);
                System.arraycopy(v, 0, rotationVector, 0, Math.min(v.length, 4));
                haveRotation = true;
                break;
            default:
                break;
        }
    }

    @Override
    public void onAccuracyChanged(Sensor sensor, int accuracy) {}
}
