package tech.krate.player;

import android.Manifest;
import android.app.AlertDialog;
import android.bluetooth.BluetoothAdapter;
import android.bluetooth.BluetoothDevice;
import android.bluetooth.BluetoothGatt;
import android.bluetooth.BluetoothGattCallback;
import android.bluetooth.BluetoothGattCharacteristic;
import android.bluetooth.BluetoothGattDescriptor;
import android.bluetooth.BluetoothGattService;
import android.bluetooth.BluetoothManager;
import android.bluetooth.BluetoothProfile;
import android.bluetooth.le.BluetoothLeScanner;
import android.bluetooth.le.ScanCallback;
import android.bluetooth.le.ScanRecord;
import android.bluetooth.le.ScanResult;
import android.bluetooth.le.ScanSettings;
import android.content.Context;
import android.content.DialogInterface;
import android.os.Build;
import android.os.Handler;
import android.os.Looper;
import android.os.ParcelUuid;
import android.util.Log;
import android.util.TypedValue;
import android.view.View;
import android.widget.AdapterView;
import android.widget.ArrayAdapter;
import android.widget.LinearLayout;
import android.widget.ListView;
import android.widget.TextView;

import java.nio.charset.StandardCharsets;
import java.util.ArrayDeque;
import java.util.ArrayList;
import java.util.HashMap;
import java.util.List;
import java.util.Locale;
import java.util.UUID;

/**
 * A Bluetooth LE device the person picks (krate:ui/bluetooth, IC-917), on
 * the platform's own BluetoothLeScanner and BluetoothGatt -- the same
 * meaning as the iOS host:
 *
 * - An app never scans. choose() shows Krate's own list (a dialog the app
 *   cannot draw) of nearby devices whose advertisement offers the service,
 *   filled as reports come in -- every report, not the first, since a
 *   device's first one can lack its service list -- plus devices already
 *   connected to the phone that offer it. The one the person taps is
 *   connected and its services found; only it gets a handle.
 * - A connect or an exchange that does not answer in 15 s is taken back,
 *   not left pending.
 *
 * Every call returns at once; the answer is polled (Rust keeps the event
 * loop running meanwhile). Answers are a status byte, then data: 0 done,
 * 1 not allowed, 2 unavailable, 3 not found, 4 disconnected, 5 failed
 * (then the reason in UTF-8), 6 cancelled.
 */
final class Ble {
    private Ble() {}

    static final byte OK = 0;
    static final byte NOT_ALLOWED = 1;
    static final byte UNAVAILABLE = 2;
    static final byte NOT_FOUND = 3;
    static final byte DISCONNECTED = 4;
    static final byte FAILED = 5;
    static final byte CANCELLED = 6;

    /** How long one exchange with a device may take. */
    private static final long EXCHANGE_MS = 15_000;
    /** How long the list keeps looking. */
    private static final long LOOK_MS = 30_000;
    /** How long the list looks before it says nothing was found. */
    private static final long FIRST_LOOK_MS = 3_500;

    private static final UUID CCCD = UUID.fromString("00002902-0000-1000-8000-00805f9b34fb");
    private static final String BASE = "-0000-1000-8000-00805F9B34FB";

    private static final Object LOCK = new Object();
    private static final Handler MAIN = new Handler(Looper.getMainLooper());

    /** A device handed out, by handle. */
    private static final class Device {
        long handle;
        String name;
        BluetoothGatt gatt;
        boolean connected;
        final ArrayDeque<byte[]> notified = new ArrayDeque<>();
    }

    private static final HashMap<Long, Device> DEVICES = new HashMap<>();
    private static long nextHandle;

    // The choose in progress.
    private static byte[] chooseAnswer;
    private static boolean choosing;
    private static ScanCallback scan;
    private static AlertDialog sheet;
    private static Device connecting;
    private static long connectStarted;

    // The exchange in progress.
    private static byte[] opAnswer;
    private static long opStarted;
    private static String opKey;

    static String[] permissions() {
        if (Build.VERSION.SDK_INT >= 31) {
            return new String[] {
                Manifest.permission.BLUETOOTH_SCAN, Manifest.permission.BLUETOOTH_CONNECT,
            };
        }
        return new String[] {Manifest.permission.ACCESS_FINE_LOCATION};
    }

    private static BluetoothAdapter adapter(Context context) {
        BluetoothManager manager =
                (BluetoothManager) context.getSystemService(Context.BLUETOOTH_SERVICE);
        return manager == null ? null : manager.getAdapter();
    }

    static boolean available(Context context) {
        return adapter(context) != null;
    }

    /** "180D" for the standard form of a standard UUID, else upper case. */
    static String shortUuid(String uuid) {
        String upper = uuid.trim().toUpperCase(Locale.ROOT);
        if (upper.endsWith(BASE) && upper.length() == 36 && upper.startsWith("0000")) {
            return upper.substring(4, 8);
        }
        return upper;
    }

    static UUID parseUuid(String uuid) {
        String s = uuid.trim();
        try {
            if (s.length() == 4) {
                return UUID.fromString("0000" + s + BASE);
            }
            if (s.length() == 8) {
                return UUID.fromString(s + BASE);
            }
            return UUID.fromString(s);
        } catch (IllegalArgumentException e) {
            return null;
        }
    }

    private static byte[] answer(byte status, String text) {
        byte[] body = text == null ? new byte[0] : text.getBytes(StandardCharsets.UTF_8);
        byte[] out = new byte[body.length + 1];
        out[0] = status;
        System.arraycopy(body, 0, out, 1, body.length);
        return out;
    }

    /**
     * Start choosing: ASKING while the phone's own prompt is up (ask
     * again), a status byte's answer when it cannot start, null once the
     * list is up (then poll chooseAnswer()).
     */
    @SuppressWarnings("MissingPermission")
    static byte[] choose(final KrateActivity activity, String service, final String title) {
        final UUID wanted = parseUuid(service);
        if (wanted == null) {
            return answer(NOT_FOUND, null);
        }
        final BluetoothAdapter bt = adapter(activity);
        if (bt == null) {
            return answer(UNAVAILABLE, null);
        }
        int asked = activity.krateAsk(permissions(), true);
        if (asked == KrateActivity.ASKING) {
            return new byte[] {(byte) 0xff};
        }
        if (asked == KrateActivity.REFUSED) {
            return answer(NOT_ALLOWED, null);
        }
        if (!bt.isEnabled()) {
            return answer(UNAVAILABLE, null);
        }
        final BluetoothLeScanner scanner = bt.getBluetoothLeScanner();
        if (scanner == null) {
            return answer(UNAVAILABLE, null);
        }
        synchronized (LOCK) {
            choosing = true;
            chooseAnswer = null;
            connecting = null;
        }
        MAIN.post(new Runnable() {
            @Override
            public void run() {
                showSheet(activity, bt, scanner, wanted, title);
            }
        });
        return null;
    }

    @SuppressWarnings("MissingPermission")
    private static void showSheet(final KrateActivity activity, final BluetoothAdapter bt,
            final BluetoothLeScanner scanner, final UUID wanted, String title) {
        final ArrayList<BluetoothDevice> found = new ArrayList<>();
        final ArrayAdapter<String> names =
                new ArrayAdapter<>(activity, android.R.layout.simple_list_item_1);
        int pad = (int) TypedValue.applyDimension(TypedValue.COMPLEX_UNIT_DIP, 24,
                activity.getResources().getDisplayMetrics());
        final TextView status = new TextView(activity);
        status.setText("Looking for devices nearby…");
        status.setPadding(pad, pad / 2, pad, pad / 3);
        ListView list = new ListView(activity);
        list.setAdapter(names);
        // Rows carry 16 dp of their own: line their names up with the text.
        int rowPad = pad - (int) TypedValue.applyDimension(TypedValue.COMPLEX_UNIT_DIP, 16,
                activity.getResources().getDisplayMetrics());
        list.setPadding(rowPad, 0, rowPad, 0);
        LinearLayout layout = new LinearLayout(activity);
        layout.setOrientation(LinearLayout.VERTICAL);
        layout.addView(status);
        layout.addView(list);

        // Devices already connected to the phone do not advertise: list the
        // ones that say they offer the service.
        BluetoothManager manager =
                (BluetoothManager) activity.getSystemService(Context.BLUETOOTH_SERVICE);
        try {
            for (BluetoothDevice device : manager.getConnectedDevices(BluetoothProfile.GATT)) {
                ParcelUuid[] uuids = device.getUuids();
                boolean offers = false;
                if (uuids != null) {
                    for (ParcelUuid uuid : uuids) {
                        offers |= wanted.equals(uuid.getUuid());
                    }
                }
                if (offers) {
                    found.add(device);
                    names.add(nameOf(device, null));
                }
            }
        } catch (SecurityException e) {
            Log.w(KrateActivity.TAG, "bluetooth: connected devices: " + e);
        }

        final ScanCallback callback = new ScanCallback() {
            @Override
            public void onScanResult(int type, ScanResult result) {
                ScanRecord record = result.getScanRecord();
                List<ParcelUuid> offered = record == null ? null : record.getServiceUuids();
                if (offered == null || !offered.contains(new ParcelUuid(wanted))) {
                    return;
                }
                BluetoothDevice device = result.getDevice();
                int at = found.indexOf(device);
                String name = nameOf(device, record);
                if (at < 0) {
                    found.add(device);
                    names.add(name);
                    status.setText("Only the device you pick is shared with the app, and only"
                            + " while it is open.");
                    Log.i(KrateActivity.TAG, "bluetooth: found " + name);
                } else if (!name.equals(names.getItem(at))) {
                    names.remove(names.getItem(at));
                    names.insert(name, at);
                }
            }

            @Override
            public void onScanFailed(int code) {
                Log.w(KrateActivity.TAG, "bluetooth: scan failed " + code);
                status.setText("Bluetooth could not look for devices (" + code + ").");
            }
        };
        ScanSettings settings = new ScanSettings.Builder()
                .setScanMode(ScanSettings.SCAN_MODE_LOW_LATENCY)
                .setCallbackType(ScanSettings.CALLBACK_TYPE_ALL_MATCHES)
                .build();
        try {
            scanner.startScan(null, settings, callback);
            Log.i(KrateActivity.TAG, "bluetooth: looking for " + shortUuid(wanted.toString()));
        } catch (SecurityException e) {
            finishChoose(answer(NOT_ALLOWED, null));
            return;
        }
        synchronized (LOCK) {
            scan = callback;
        }
        final Runnable stopLooking = new Runnable() {
            @Override
            public void run() {
                stopScan(scanner);
            }
        };
        MAIN.postDelayed(new Runnable() {
            @Override
            public void run() {
                if (found.isEmpty()) {
                    status.setText("No device offering this was found nearby. Check it is on"
                            + " and close by.");
                }
            }
        }, FIRST_LOOK_MS);
        MAIN.postDelayed(stopLooking, LOOK_MS);

        AlertDialog.Builder builder = new AlertDialog.Builder(activity)
                .setTitle(title == null || title.isEmpty() ? "Pick a device" : title)
                .setView(layout)
                .setNegativeButton("Cancel", new DialogInterface.OnClickListener() {
                    @Override
                    public void onClick(DialogInterface dialog, int which) {
                        dialog.dismiss();
                    }
                })
                .setOnDismissListener(new DialogInterface.OnDismissListener() {
                    @Override
                    public void onDismiss(DialogInterface dialog) {
                        MAIN.removeCallbacks(stopLooking);
                        stopScan(scanner);
                        synchronized (LOCK) {
                            sheet = null;
                            if (connecting == null && choosing) {
                                choosing = false;
                                chooseAnswer = answer(CANCELLED, null);
                            }
                        }
                    }
                });
        final AlertDialog dialog = builder.create();
        list.setOnItemClickListener(new AdapterView.OnItemClickListener() {
            @Override
            public void onItemClick(AdapterView<?> parent, View view, int position, long id) {
                pick(activity, scanner, stopLooking, dialog, found.get(position),
                        names.getItem(position));
            }
        });
        synchronized (LOCK) {
            sheet = dialog;
        }
        dialog.show();
    }

    private static void pick(KrateActivity activity, BluetoothLeScanner scanner,
            Runnable stopLooking, AlertDialog dialog, BluetoothDevice device, String name) {
        synchronized (LOCK) {
            Device picked = new Device();
            picked.handle = ++nextHandle;
            picked.name = name;
            connecting = picked;
            connectStarted = System.currentTimeMillis();
        }
        MAIN.removeCallbacks(stopLooking);
        stopScan(scanner);
        dialog.dismiss();
        connect(activity, device);
    }

    @SuppressWarnings("MissingPermission")
    private static void stopScan(BluetoothLeScanner scanner) {
        ScanCallback callback;
        synchronized (LOCK) {
            callback = scan;
            scan = null;
        }
        if (callback != null) {
            try {
                scanner.stopScan(callback);
                Log.i(KrateActivity.TAG, "bluetooth: stopped looking");
            } catch (SecurityException | IllegalStateException e) {
                Log.w(KrateActivity.TAG, "bluetooth: stop scan: " + e);
            }
        }
    }

    private static String nameOf(BluetoothDevice device, ScanRecord record) {
        String name = record == null ? null : record.getDeviceName();
        if (name == null || name.trim().isEmpty()) {
            try {
                name = device.getName();
            } catch (SecurityException e) {
                name = null;
            }
        }
        return name == null || name.trim().isEmpty() ? "Unnamed device" : name.trim();
    }

    @SuppressWarnings("MissingPermission")
    private static void connect(KrateActivity activity, BluetoothDevice device) {
        final Device picked;
        synchronized (LOCK) {
            picked = connecting;
        }
        Log.i(KrateActivity.TAG, "bluetooth: connecting to " + picked.name);
        try {
            picked.gatt = device.connectGatt(activity, false, new Callback(picked),
                    BluetoothDevice.TRANSPORT_LE);
        } catch (SecurityException e) {
            finishChoose(answer(NOT_ALLOWED, null));
            return;
        }
        if (picked.gatt == null) {
            finishChoose(answer(FAILED, "it could not be reached"));
        }
    }

    private static void finishChoose(byte[] result) {
        synchronized (LOCK) {
            if (!choosing) {
                return;
            }
            choosing = false;
            chooseAnswer = result;
            connecting = null;
        }
    }

    /** Null while choosing; then the answer: OK carries the handle (8 bytes) and the name. */
    @SuppressWarnings("MissingPermission")
    static byte[] chooseAnswer() {
        synchronized (LOCK) {
            if (choosing && connecting != null
                    && System.currentTimeMillis() - connectStarted > EXCHANGE_MS) {
                // A connect left pending keeps trying for ever: take it back.
                Device late = connecting;
                connecting = null;
                choosing = false;
                chooseAnswer = answer(FAILED, "it did not answer");
                if (late.gatt != null) {
                    try {
                        late.gatt.disconnect();
                        late.gatt.close();
                    } catch (SecurityException e) {
                        Log.w(KrateActivity.TAG, "bluetooth: " + e);
                    }
                }
                Log.i(KrateActivity.TAG, "bluetooth: connect timed out");
            }
            return choosing ? null : chooseAnswer;
        }
    }

    private static Device device(long handle) {
        synchronized (LOCK) {
            return DEVICES.get(handle);
        }
    }

    private static BluetoothGattCharacteristic find(Device d, String service, String ch) {
        UUID s = parseUuid(service);
        UUID c = parseUuid(ch);
        if (s == null || c == null || d.gatt == null) {
            return null;
        }
        BluetoothGattService found = d.gatt.getService(s);
        return found == null ? null : found.getCharacteristic(c);
    }

    /**
     * Start an exchange: kind 0 read, 1 write, 2 subscribe. Its answer comes
     * from opAnswer(). Only one exchange runs at a time, as Android's GATT
     * allows.
     */
    @SuppressWarnings({"MissingPermission", "deprecation"})
    static void op(long handle, int kind, String service, String characteristic, byte[] data,
            boolean withResponse) {
        synchronized (LOCK) {
            opAnswer = null;
            opStarted = System.currentTimeMillis();
            opKey = null;
        }
        Device d = device(handle);
        if (d == null || !d.connected) {
            finishOp(null, answer(DISCONNECTED, null));
            return;
        }
        BluetoothGattCharacteristic c = find(d, service, characteristic);
        if (c == null) {
            finishOp(null, answer(NOT_FOUND, null));
            return;
        }
        String key = handle + "/" + c.getService().getUuid() + "/" + c.getUuid();
        synchronized (LOCK) {
            opKey = key;
        }
        boolean started;
        try {
            if (kind == 0) {
                started = d.gatt.readCharacteristic(c);
            } else if (kind == 1) {
                c.setValue(data);
                c.setWriteType(withResponse ? BluetoothGattCharacteristic.WRITE_TYPE_DEFAULT
                        : BluetoothGattCharacteristic.WRITE_TYPE_NO_RESPONSE);
                started = d.gatt.writeCharacteristic(c);
                if (started && !withResponse) {
                    finishOp(key, answer(OK, null));
                }
            } else {
                d.gatt.setCharacteristicNotification(c, true);
                BluetoothGattDescriptor cccd = c.getDescriptor(CCCD);
                if (cccd == null) {
                    finishOp(key, answer(OK, null));
                    return;
                }
                boolean indicate = (c.getProperties()
                        & BluetoothGattCharacteristic.PROPERTY_NOTIFY) == 0;
                cccd.setValue(indicate ? BluetoothGattDescriptor.ENABLE_INDICATION_VALUE
                        : BluetoothGattDescriptor.ENABLE_NOTIFICATION_VALUE);
                started = d.gatt.writeDescriptor(cccd);
            }
        } catch (SecurityException e) {
            finishOp(key, answer(NOT_ALLOWED, null));
            return;
        }
        if (!started) {
            finishOp(key, answer(FAILED, "the device is busy"));
        }
    }

    private static void finishOp(String key, byte[] result) {
        synchronized (LOCK) {
            if (opAnswer != null || (key != null && opKey != null && !key.equals(opKey))) {
                return;
            }
            opAnswer = result;
        }
    }

    /** Null while the exchange runs; then its answer (read: OK and the value). */
    static byte[] opAnswer() {
        synchronized (LOCK) {
            if (opAnswer == null && System.currentTimeMillis() - opStarted > EXCHANGE_MS) {
                opAnswer = answer(FAILED, "it did not answer in time");
            }
            return opAnswer;
        }
    }

    /** Values sent since the last ask: each service, characteristic, value. */
    static byte[][] notifications(long handle) {
        Device d = device(handle);
        if (d == null) {
            return new byte[0][];
        }
        synchronized (LOCK) {
            byte[][] out = d.notified.toArray(new byte[0][]);
            d.notified.clear();
            return out;
        }
    }

    static boolean connected(long handle) {
        Device d = device(handle);
        synchronized (LOCK) {
            return d != null && d.connected;
        }
    }

    @SuppressWarnings("MissingPermission")
    static void disconnect(long handle) {
        Device d;
        synchronized (LOCK) {
            d = DEVICES.remove(handle);
        }
        if (d != null && d.gatt != null) {
            try {
                d.gatt.disconnect();
                d.gatt.close();
                Log.i(KrateActivity.TAG, "bluetooth: let " + d.name + " go");
            } catch (SecurityException e) {
                Log.w(KrateActivity.TAG, "bluetooth: " + e);
            }
        }
    }

    /** The app closed: let every device go, and take the list down. */
    static void disconnectAll() {
        ArrayList<Long> handles;
        final AlertDialog up;
        synchronized (LOCK) {
            handles = new ArrayList<>(DEVICES.keySet());
            up = sheet;
            choosing = false;
        }
        for (long handle : handles) {
            disconnect(handle);
        }
        if (up != null) {
            MAIN.post(new Runnable() {
                @Override
                public void run() {
                    up.dismiss();
                }
            });
        }
    }

    /** One picked device's GATT callbacks, on Android's Bluetooth threads. */
    private static final class Callback extends BluetoothGattCallback {
        private final Device device;

        Callback(Device device) {
            this.device = device;
        }

        @Override
        @SuppressWarnings("MissingPermission")
        public void onConnectionStateChange(BluetoothGatt gatt, int status, int state) {
            Log.i(KrateActivity.TAG, "bluetooth: " + device.name + " state " + state
                    + " status " + status);
            if (state == BluetoothProfile.STATE_CONNECTED) {
                synchronized (LOCK) {
                    device.connected = true;
                }
                try {
                    if (!gatt.discoverServices()) {
                        finishChoose(answer(FAILED, "it did not list its services"));
                    }
                } catch (SecurityException e) {
                    finishChoose(answer(NOT_ALLOWED, null));
                }
                return;
            }
            boolean wasPicking;
            synchronized (LOCK) {
                device.connected = false;
                wasPicking = connecting == device;
            }
            if (wasPicking) {
                finishChoose(answer(FAILED, "it would not connect (" + status + ")"));
                try {
                    gatt.close();
                } catch (SecurityException e) {
                    Log.w(KrateActivity.TAG, "bluetooth: " + e);
                }
            } else {
                // Gone: its exchanges answer disconnected, and the phone's
                // connection slot is given back (Android has only a few).
                finishOp(null, answer(DISCONNECTED, null));
                try {
                    gatt.close();
                } catch (SecurityException e) {
                    Log.w(KrateActivity.TAG, "bluetooth: " + e);
                }
            }
        }

        @Override
        public void onServicesDiscovered(BluetoothGatt gatt, int status) {
            if (status != BluetoothGatt.GATT_SUCCESS) {
                finishChoose(answer(FAILED, "it did not list its services (" + status + ")"));
                return;
            }
            byte[] done;
            synchronized (LOCK) {
                if (connecting != device) {
                    return;
                }
                DEVICES.put(device.handle, device);
                byte[] name = device.name.getBytes(StandardCharsets.UTF_8);
                done = new byte[9 + name.length];
                done[0] = OK;
                for (int i = 0; i < 8; i++) {
                    done[1 + i] = (byte) (device.handle >>> (56 - 8 * i));
                }
                System.arraycopy(name, 0, done, 9, name.length);
            }
            Log.i(KrateActivity.TAG, "bluetooth: " + device.name + " connected, "
                    + gatt.getServices().size() + " services");
            finishChoose(done);
        }

        private String key(BluetoothGattCharacteristic c) {
            return device.handle + "/" + c.getService().getUuid() + "/" + c.getUuid();
        }

        @Override
        @SuppressWarnings("deprecation")
        public void onCharacteristicRead(BluetoothGatt gatt, BluetoothGattCharacteristic c,
                int status) {
            if (status != BluetoothGatt.GATT_SUCCESS) {
                finishOp(key(c), answer(FAILED, "it refused the read (" + status + ")"));
                return;
            }
            byte[] value = c.getValue() == null ? new byte[0] : c.getValue();
            byte[] out = new byte[value.length + 1];
            out[0] = OK;
            System.arraycopy(value, 0, out, 1, value.length);
            finishOp(key(c), out);
        }

        @Override
        public void onCharacteristicWrite(BluetoothGatt gatt, BluetoothGattCharacteristic c,
                int status) {
            finishOp(key(c), status == BluetoothGatt.GATT_SUCCESS ? answer(OK, null)
                    : answer(FAILED, "it refused the write (" + status + ")"));
        }

        @Override
        public void onDescriptorWrite(BluetoothGatt gatt, BluetoothGattDescriptor d,
                int status) {
            finishOp(key(d.getCharacteristic()), status == BluetoothGatt.GATT_SUCCESS
                    ? answer(OK, null)
                    : answer(FAILED, "it refused to send changes (" + status + ")"));
        }

        @Override
        @SuppressWarnings("deprecation")
        public void onCharacteristicChanged(BluetoothGatt gatt, BluetoothGattCharacteristic c) {
            byte[] value = c.getValue() == null ? new byte[0] : c.getValue();
            byte[] service = shortUuid(c.getService().getUuid().toString())
                    .getBytes(StandardCharsets.UTF_8);
            byte[] ch = shortUuid(c.getUuid().toString()).getBytes(StandardCharsets.UTF_8);
            byte[] entry = new byte[2 + service.length + ch.length + value.length];
            entry[0] = (byte) service.length;
            System.arraycopy(service, 0, entry, 1, service.length);
            entry[1 + service.length] = (byte) ch.length;
            System.arraycopy(ch, 0, entry, 2 + service.length, ch.length);
            System.arraycopy(value, 0, entry, 2 + service.length + ch.length, value.length);
            synchronized (LOCK) {
                // Bounded: an app that never collects keeps the newest.
                if (device.notified.size() >= 256) {
                    device.notified.removeFirst();
                }
                device.notified.addLast(entry);
            }
        }
    }
}
