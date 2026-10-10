package tech.krate.player;

import android.app.NativeActivity;
import android.content.ActivityNotFoundException;
import android.content.Intent;
import android.content.pm.PackageManager;
import android.net.Uri;
import android.util.Log;

import java.util.HashMap;

/**
 * The Krate player's one activity: NativeActivity, which loads
 * libkrate_player.so and runs android_main, plus the few things only an
 * activity can be told.
 *
 * Everything Krate does happens in Rust. This class exists because some
 * answers Android gives only to an activity -- what the person picked in the
 * system's file picker (onActivityResult) -- and a NativeActivity drops them.
 * The native side never waits on the UI thread: every method here returns at
 * once, and Rust asks again (while it keeps the event loop running) until the
 * answer is in.
 */
public class KrateActivity extends NativeActivity {
    static final String TAG = "krate";

    /** The request code of Krate's picker; the low 16 bits are all Android keeps. */
    private static final int PICK = 0x4b01;
    /** The request code of the phone's permission prompts. */
    private static final int ASK = 0x4b02;

    /** krateAsk's answers. */
    static final int GRANTED = 0;
    static final int REFUSED = 1;
    static final int ASKING = 2;

    /** Permission sets asked for this run, and whether the prompt has answered. */
    private final HashMap<String, Boolean> asked = new HashMap<>();

    private final Object lock = new Object();
    /** True while the system's picker is up. */
    private boolean picking;
    /** What the picker answered: a content URI, or "" for cancelled. */
    private String picked = "";

    @Override
    protected void onActivityResult(int request, int result, Intent data) {
        super.onActivityResult(request, result, data);
        if (request != PICK) {
            return;
        }
        Uri uri = (result == RESULT_OK && data != null) ? data.getData() : null;
        synchronized (lock) {
            picked = uri == null ? "" : uri.toString();
            picking = false;
        }
    }

    @Override
    public void onRequestPermissionsResult(int request, String[] permissions, int[] results) {
        super.onRequestPermissionsResult(request, permissions, results);
        if (request != ASK) {
            return;
        }
        synchronized (lock) {
            asked.put(String.join(",", permissions), Boolean.TRUE);
        }
    }

    /**
     * Whether the phone lets Krate use these permissions (any of them, or
     * all when all is true), asking the person the first time this run.
     * The app's own grant was already checked on Krate's sheet; this is the
     * phone's prompt behind it. GRANTED, REFUSED once the prompt answered
     * no, or ASKING while it is up.
     */
    public int krateAsk(final String[] permissions, boolean all) {
        int granted = 0;
        for (String permission : permissions) {
            if (checkSelfPermission(permission) == PackageManager.PERMISSION_GRANTED) {
                granted++;
            }
        }
        if (all ? granted == permissions.length : granted > 0) {
            return GRANTED;
        }
        String key = String.join(",", permissions);
        synchronized (lock) {
            Boolean answered = asked.get(key);
            if (answered != null) {
                return answered ? REFUSED : ASKING;
            }
            asked.put(key, Boolean.FALSE);
        }
        Log.i(TAG, "asking the phone for " + key);
        runOnUiThread(new Runnable() {
            @Override
            public void run() {
                requestPermissions(permissions, ASK);
            }
        });
        return ASKING;
    }

    /**
     * Show the system's own picker. kind 0 opens a file, 1 opens a folder,
     * 2 asks where to save a new file called name. Answered through
     * {@link #kratePickAnswer()}.
     */
    public void kratePick(final int kind, final String mime, final String name) {
        synchronized (lock) {
            picking = true;
            picked = "";
        }
        runOnUiThread(new Runnable() {
            @Override
            public void run() {
                Intent intent;
                if (kind == 1) {
                    intent = new Intent(Intent.ACTION_OPEN_DOCUMENT_TREE);
                } else if (kind == 2) {
                    intent = new Intent(Intent.ACTION_CREATE_DOCUMENT);
                    intent.addCategory(Intent.CATEGORY_OPENABLE);
                    intent.setType(mime == null || mime.isEmpty() ? Documents.mimeOf(name) : mime);
                    intent.putExtra(Intent.EXTRA_TITLE, name);
                } else {
                    intent = new Intent(Intent.ACTION_OPEN_DOCUMENT);
                    intent.addCategory(Intent.CATEGORY_OPENABLE);
                    intent.setType(mime == null || mime.isEmpty() ? "*/*" : mime);
                }
                try {
                    startActivityForResult(intent, PICK);
                } catch (ActivityNotFoundException e) {
                    Log.w(TAG, "no picker on this phone: " + e);
                    synchronized (lock) {
                        picking = false;
                        picked = "";
                    }
                }
            }
        });
    }

    /** Null while the picker is up; then the picked URI, or "" for cancelled. */
    public String kratePickAnswer() {
        synchronized (lock) {
            return picking ? null : picked;
        }
    }

    /** Copy a picked file into dir under its own name; the path, or null. */
    public String krateCopyIn(String uri, String dir) {
        return Documents.copyIn(this, Uri.parse(uri), dir);
    }

    /** Copy a picked folder into dir; the copy's path, or null. */
    public String krateMirror(String tree, String dir, long maxBytes) {
        return Documents.mirror(this, Uri.parse(tree), dir, maxBytes);
    }

    /** Put what the app changed in a mirrored folder back where it came from. */
    public int krateSyncBack(String root) {
        return Documents.syncBack(this, root);
    }

    /** Whether the phone can tap back (krate:ui/haptics). */
    public boolean krateHapticsAvailable() {
        return Haptics.available(this);
    }

    /** Tap back with feel, or an impact at strength when feel is -1. */
    public boolean krateHaptic(int feel, float strength) {
        return Haptics.play(this, feel, strength);
    }

    /** Whether the phone can tell how it is held (krate:ui/motion). */
    public boolean krateMotionAvailable() {
        return Motion.available(this);
    }

    /** The latest motion reading, starting the sensors on the first ask. */
    public float[] krateMotion() {
        return Motion.read(this);
    }

    /** Stop the motion sensors. */
    public void krateMotionStop() {
        Motion.stop();
    }

    /** Whether the phone can tell where it is (krate:ui/location). */
    public boolean krateLocationAvailable() {
        return Where.available(this);
    }

    /** The latest fix, asking and starting on the first call; see Where.read. */
    public double[] krateLocation() {
        return Where.read(this);
    }

    /** Stop looking for where the phone is. */
    public void krateLocationStop() {
        Where.stop();
    }

    /** Whether the phone has Bluetooth (krate:ui/bluetooth). */
    public boolean krateBtAvailable() {
        return Ble.available(this);
    }

    /** Start Krate's device list; see Ble.choose. */
    public byte[] krateBtChoose(String service, String title) {
        return Ble.choose(this, service, title);
    }

    /** Null while the person is choosing; then the pick; see Ble.chooseAnswer. */
    public byte[] krateBtChooseAnswer() {
        return Ble.chooseAnswer();
    }

    /** Start a read (0), write (1) or subscribe (2) on a picked device. */
    public void krateBtOp(long device, int kind, String service, String characteristic,
            byte[] data, boolean withResponse) {
        Ble.op(device, kind, service, characteristic, data, withResponse);
    }

    /** Null while the exchange runs; then its answer. */
    public byte[] krateBtOpAnswer() {
        return Ble.opAnswer();
    }

    /** Values a device sent since the last ask. */
    public byte[][] krateBtNotifications(long device) {
        return Ble.notifications(device);
    }

    /** Whether a picked device is still connected. */
    public boolean krateBtConnected(long device) {
        return Ble.connected(device);
    }

    /** Let a picked device go. */
    public void krateBtDisconnect(long device) {
        Ble.disconnect(device);
    }

    /** The app closed: let every device go. */
    public void krateBtDisconnectAll() {
        Ble.disconnectAll();
    }

    // IC-918: the clipboard, links, notifications, speaking aloud, the
    // microphone's and camera's prompts, and the camera itself.

    public String krateClipRead() {
        return Phone.readClip(this);
    }

    public boolean krateClipWrite(String text) {
        return Phone.writeClip(this, text);
    }

    public String krateOpenUrl(String url) {
        return Phone.openUrl(this, url);
    }

    /** The phone's prompt for posting notifications (Android 13 and up). */
    public int krateNotifyAsk() {
        String[] permissions = Phone.notifyPermissions();
        return permissions.length == 0 ? GRANTED : krateAsk(permissions, true);
    }

    public String krateNotify(String app, String title, String body) {
        return Phone.notify(this, app, title, body);
    }

    public int krateMicAsk() {
        return krateAsk(new String[] {"android.permission.RECORD_AUDIO"}, true);
    }

    public int krateCameraAsk() {
        return krateAsk(new String[] {"android.permission.CAMERA"}, true);
    }

    public int krateSpeechReady() {
        return Phone.speechReady(this);
    }

    public String[] krateVoices() {
        return Phone.voices();
    }

    public String krateSay(String text, String voice, float rate) {
        return Phone.say(text, voice, rate);
    }

    public void krateStopSpeaking() {
        Phone.stopSpeaking();
    }

    public boolean krateSpeaking() {
        return Phone.isSpeaking();
    }

    public String[] krateCameras() {
        return Cam.devices(this);
    }

    public String krateCameraOpen(String id, int width, int height, int fps) {
        return Cam.open(this, id, width, height, fps);
    }

    public int[] krateCameraInfo() {
        return Cam.info();
    }

    public byte[] krateCameraFrame() {
        return Cam.frame();
    }

    public String krateCameraError() {
        return Cam.error();
    }

    public void krateCameraClose() {
        Cam.close();
    }

    /** The display's rotation, 0..3 (ROTATION_0 to ROTATION_270). */
    @SuppressWarnings("deprecation")
    public int krateDisplayRotation() {
        return getWindowManager().getDefaultDisplay().getRotation();
    }

    /** Write the file at path into the document at uri; true when it all went. */
    public boolean krateCopyOut(String path, String uri) {
        return Documents.copyOut(this, path, Uri.parse(uri));
    }
}
