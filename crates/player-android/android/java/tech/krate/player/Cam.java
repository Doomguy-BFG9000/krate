package tech.krate.player;

import android.content.Context;
import android.graphics.ImageFormat;
import android.hardware.camera2.CameraAccessException;
import android.hardware.camera2.CameraCaptureSession;
import android.hardware.camera2.CameraCharacteristics;
import android.hardware.camera2.CameraDevice;
import android.hardware.camera2.CameraManager;
import android.hardware.camera2.CaptureRequest;
import android.hardware.camera2.params.StreamConfigurationMap;
import android.media.Image;
import android.media.ImageReader;
import android.os.Handler;
import android.os.HandlerThread;
import android.util.Log;
import android.util.Range;
import android.util.Size;

import java.nio.ByteBuffer;
import java.util.ArrayList;
import java.util.Collections;

/**
 * The camera on Android (krate:camera, IC-918): Camera2 into an ImageReader,
 * each frame packed as tight I420 (Y, then U, then V) for Rust, which turns
 * it to RGBA the right way up for the screen. Only the newest frame is kept.
 */
@SuppressWarnings("MissingPermission")
final class Cam {
    private Cam() {}

    private static final Object LOCK = new Object();
    private static HandlerThread thread;
    private static Handler handler;
    private static CameraDevice device;
    private static CameraCaptureSession session;
    private static ImageReader reader;
    private static byte[] latest;
    private static long frames;
    private static String error;
    private static int width;
    private static int height;
    private static int orientation;
    private static boolean front;

    /** Each camera as id and label, tab separated, back cameras first. */
    static String[] devices(Context context) {
        CameraManager manager = (CameraManager) context.getSystemService(Context.CAMERA_SERVICE);
        ArrayList<String> back = new ArrayList<>();
        ArrayList<String> rest = new ArrayList<>();
        try {
            for (String id : manager.getCameraIdList()) {
                Integer facing = manager.getCameraCharacteristics(id)
                        .get(CameraCharacteristics.LENS_FACING);
                if (facing != null && facing == CameraCharacteristics.LENS_FACING_BACK) {
                    back.add(id + "\tBack camera");
                } else if (facing != null && facing == CameraCharacteristics.LENS_FACING_FRONT) {
                    rest.add(id + "\tFront camera");
                } else {
                    rest.add(id + "\tCamera " + id);
                }
            }
        } catch (CameraAccessException | RuntimeException e) {
            Log.w(KrateActivity.TAG, "camera list: " + e);
        }
        back.addAll(rest);
        return back.toArray(new String[0]);
    }

    /**
     * Open camera id (the first back one when empty) as close to w x h as
     * it offers. Null once it is opening; else why not. The size it chose is
     * in info().
     */
    static String open(Context context, String id, int w, int h, int fps) {
        close();
        CameraManager manager = (CameraManager) context.getSystemService(Context.CAMERA_SERVICE);
        try {
            String chosen = id;
            if (chosen == null || chosen.isEmpty()) {
                String[] all = devices(context);
                if (all.length == 0) {
                    return "this phone has no camera";
                }
                chosen = all[0].substring(0, all[0].indexOf('\t'));
            }
            CameraCharacteristics c = manager.getCameraCharacteristics(chosen);
            StreamConfigurationMap map = c.get(CameraCharacteristics.SCALER_STREAM_CONFIGURATION_MAP);
            if (map == null) {
                return "this camera gives no pictures";
            }
            Size best = null;
            long want = (long) w * h;
            for (Size s : map.getOutputSizes(ImageFormat.YUV_420_888)) {
                long d = Math.abs((long) s.getWidth() * s.getHeight() - want);
                if (best == null
                        || d < Math.abs((long) best.getWidth() * best.getHeight() - want)) {
                    best = s;
                }
            }
            if (best == null) {
                return "this camera gives no pictures Krate can read";
            }
            Integer sensor = c.get(CameraCharacteristics.SENSOR_ORIENTATION);
            Integer facing = c.get(CameraCharacteristics.LENS_FACING);
            final Range<Integer> range = fpsRange(c, fps);
            synchronized (LOCK) {
                width = best.getWidth();
                height = best.getHeight();
                orientation = sensor == null ? 0 : sensor;
                front = facing != null && facing == CameraCharacteristics.LENS_FACING_FRONT;
                latest = null;
                frames = 0;
                error = null;
            }
            thread = new HandlerThread("krate-camera");
            thread.start();
            handler = new Handler(thread.getLooper());
            reader = ImageReader.newInstance(width, height, ImageFormat.YUV_420_888, 2);
            reader.setOnImageAvailableListener(new ImageReader.OnImageAvailableListener() {
                @Override
                public void onImageAvailable(ImageReader r) {
                    Image image = r.acquireLatestImage();
                    if (image == null) {
                        return;
                    }
                    try {
                        byte[] packed = pack(image);
                        synchronized (LOCK) {
                            latest = packed;
                            if (frames++ == 0) {
                                Log.i(KrateActivity.TAG, "camera: first frame " + width + "x"
                                        + height + ", sensor at " + orientation);
                            }
                        }
                    } finally {
                        image.close();
                    }
                }
            }, handler);
            manager.openCamera(chosen, new CameraDevice.StateCallback() {
                @Override
                public void onOpened(CameraDevice opened) {
                    synchronized (LOCK) {
                        device = opened;
                    }
                    startSession(opened, range);
                }

                @Override
                public void onDisconnected(CameraDevice gone) {
                    fail("the camera was taken by another app");
                    gone.close();
                }

                @Override
                public void onError(CameraDevice broken, int code) {
                    fail("the camera stopped (" + code + ")");
                    broken.close();
                }
            }, handler);
            Log.i(KrateActivity.TAG, "camera: opening " + chosen + " at " + width + "x" + height);
            return null;
        } catch (CameraAccessException | SecurityException | IllegalArgumentException e) {
            return "the camera could not be opened: " + e.getMessage();
        }
    }

    private static Range<Integer> fpsRange(CameraCharacteristics c, int fps) {
        Range<Integer>[] ranges = c.get(CameraCharacteristics.CONTROL_AE_AVAILABLE_TARGET_FPS_RANGES);
        Range<Integer> best = null;
        if (ranges != null) {
            for (Range<Integer> r : ranges) {
                if (r.contains(fps) && (best == null || r.getLower() > best.getLower())) {
                    best = r;
                }
            }
        }
        return best;
    }

    @SuppressWarnings("deprecation")
    private static void startSession(final CameraDevice opened, final Range<Integer> range) {
        try {
            opened.createCaptureSession(Collections.singletonList(reader.getSurface()),
                    new CameraCaptureSession.StateCallback() {
                        @Override
                        public void onConfigured(CameraCaptureSession s) {
                            try {
                                CaptureRequest.Builder b =
                                        opened.createCaptureRequest(CameraDevice.TEMPLATE_PREVIEW);
                                b.addTarget(reader.getSurface());
                                if (range != null) {
                                    b.set(CaptureRequest.CONTROL_AE_TARGET_FPS_RANGE, range);
                                }
                                s.setRepeatingRequest(b.build(), null, handler);
                                synchronized (LOCK) {
                                    session = s;
                                }
                            } catch (CameraAccessException | IllegalStateException e) {
                                fail("the camera would not start: " + e.getMessage());
                            }
                        }

                        @Override
                        public void onConfigureFailed(CameraCaptureSession s) {
                            fail("the camera would not start");
                        }
                    }, handler);
        } catch (CameraAccessException | IllegalStateException e) {
            fail("the camera would not start: " + e.getMessage());
        }
    }

    private static void fail(String why) {
        Log.w(KrateActivity.TAG, "camera: " + why);
        synchronized (LOCK) {
            error = why;
        }
    }

    /** Tight I420: the Y plane, then U, then V, each without row padding. */
    private static byte[] pack(Image image) {
        int w = image.getWidth();
        int h = image.getHeight();
        int cw = (w + 1) / 2;
        int ch = (h + 1) / 2;
        byte[] out = new byte[w * h + 2 * cw * ch];
        Image.Plane[] planes = image.getPlanes();
        copyPlane(planes[0], w, h, out, 0);
        copyPlane(planes[1], cw, ch, out, w * h);
        copyPlane(planes[2], cw, ch, out, w * h + cw * ch);
        return out;
    }

    private static void copyPlane(Image.Plane plane, int w, int h, byte[] out, int at) {
        ByteBuffer buf = plane.getBuffer();
        int row = plane.getRowStride();
        int step = plane.getPixelStride();
        if (step == 1) {
            for (int y = 0; y < h; y++) {
                buf.position(y * row);
                buf.get(out, at + y * w, w);
            }
            return;
        }
        byte[] line = new byte[row];
        for (int y = 0; y < h; y++) {
            int start = y * row;
            int len = Math.min(row, buf.limit() - start);
            buf.position(start);
            buf.get(line, 0, len);
            for (int x = 0; x < w && x * step < len; x++) {
                out[at + y * w + x] = line[x * step];
            }
        }
    }

    /** {width, height, sensor orientation, 1 if front-facing, frames so far}. */
    static int[] info() {
        synchronized (LOCK) {
            return new int[] {width, height, orientation, front ? 1 : 0, (int) frames};
        }
    }

    /** The newest frame since the last ask, or null. */
    static byte[] frame() {
        synchronized (LOCK) {
            byte[] out = latest;
            latest = null;
            return out;
        }
    }

    /** Why the camera stopped, or null. */
    static String error() {
        synchronized (LOCK) {
            return error;
        }
    }

    static void close() {
        CameraCaptureSession s;
        CameraDevice d;
        synchronized (LOCK) {
            s = session;
            d = device;
            session = null;
            device = null;
            latest = null;
        }
        if (s != null) {
            s.close();
        }
        if (d != null) {
            d.close();
            Log.i(KrateActivity.TAG, "camera: closed");
        }
        if (reader != null) {
            reader.close();
            reader = null;
        }
        if (thread != null) {
            thread.quitSafely();
            thread = null;
        }
    }
}
