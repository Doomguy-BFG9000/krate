package tech.krate.player;

import android.Manifest;
import android.content.Context;
import android.location.Location;
import android.location.LocationListener;
import android.location.LocationManager;
import android.os.Build;
import android.os.Bundle;
import android.os.Looper;
import android.os.SystemClock;
import android.util.Log;

/**
 * Where the phone is (krate:ui/location, IC-917), from the platform's own
 * LocationManager -- no Google Play services. The first ask brings up the
 * phone's location prompt; once allowed, GPS and the network provider both
 * look and the better recent fix is kept. Stopped when the app closes.
 */
final class Where implements LocationListener {
    static final int HERE = 0;
    static final int NOT_ALLOWED = 1;
    static final int UNAVAILABLE = 2;
    static final int FINDING = 3;

    private static final String[] ASK = {
        Manifest.permission.ACCESS_FINE_LOCATION, Manifest.permission.ACCESS_COARSE_LOCATION,
    };

    private static Where running;

    private final LocationManager manager;
    private Location best;

    private Where(LocationManager manager) {
        this.manager = manager;
    }

    private static LocationManager manager(KrateActivity activity) {
        return (LocationManager) activity.getSystemService(Context.LOCATION_SERVICE);
    }

    static boolean available(KrateActivity activity) {
        LocationManager lm = manager(activity);
        if (lm == null) {
            return false;
        }
        if (Build.VERSION.SDK_INT >= 28) {
            return lm.isLocationEnabled();
        }
        return lm.isProviderEnabled(LocationManager.GPS_PROVIDER)
                || lm.isProviderEnabled(LocationManager.NETWORK_PROVIDER);
    }

    /**
     * {status, latitude, longitude, accuracy m, altitude m (NaN if not
     * known), speed m/s (NaN), course deg (NaN), age ms}.
     */
    static synchronized double[] read(KrateActivity activity) {
        double[] out = new double[] {FINDING, 0, 0, 0, Double.NaN, Double.NaN, Double.NaN, 0};
        LocationManager lm = manager(activity);
        if (lm == null || !available(activity)) {
            out[0] = UNAVAILABLE;
            return out;
        }
        switch (activity.krateAsk(ASK, false)) {
            case KrateActivity.REFUSED:
                out[0] = NOT_ALLOWED;
                return out;
            case KrateActivity.ASKING:
                return out;
            default:
                break;
        }
        if (running == null) {
            running = new Where(lm);
            running.start(activity);
        }
        Location fix = running.latest();
        if (fix == null) {
            return out;
        }
        out[0] = HERE;
        out[1] = fix.getLatitude();
        out[2] = fix.getLongitude();
        out[3] = fix.hasAccuracy() ? fix.getAccuracy() : 0;
        if (fix.hasAltitude()) {
            out[4] = fix.getAltitude();
        }
        if (fix.hasSpeed()) {
            out[5] = fix.getSpeed();
        }
        if (fix.hasBearing() && fix.hasSpeed() && fix.getSpeed() > 0) {
            out[6] = fix.getBearing();
        }
        long ageNanos = SystemClock.elapsedRealtimeNanos() - fix.getElapsedRealtimeNanos();
        out[7] = Math.max(0, ageNanos / 1_000_000L);
        return out;
    }

    private void start(final KrateActivity activity) {
        try {
            for (String provider : new String[] {
                    LocationManager.GPS_PROVIDER, LocationManager.NETWORK_PROVIDER}) {
                if (!manager.isProviderEnabled(provider)) {
                    continue;
                }
                Location last = manager.getLastKnownLocation(provider);
                if (last != null) {
                    onLocationChanged(last);
                }
                manager.requestLocationUpdates(provider, 1000L, 0f, this, Looper.getMainLooper());
            }
            Log.i(KrateActivity.TAG, "location: looking");
        } catch (SecurityException | IllegalArgumentException e) {
            Log.w(KrateActivity.TAG, "location could not start: " + e);
        }
    }

    private synchronized Location latest() {
        return best;
    }

    static synchronized void stop() {
        if (running != null) {
            running.manager.removeUpdates(running);
            running = null;
            Log.i(KrateActivity.TAG, "location: stopped");
        }
    }

    @Override
    public synchronized void onLocationChanged(Location fix) {
        // A newer fix wins unless it is much worse and the older one is
        // still fresh: a network fix must not push out a GPS one a moment
        // old.
        if (best == null) {
            best = fix;
            return;
        }
        long newer = fix.getElapsedRealtimeNanos() - best.getElapsedRealtimeNanos();
        boolean muchWorse = fix.getAccuracy() > best.getAccuracy() * 4;
        if (newer > 0 && (!muchWorse || newer > 10_000_000_000L)) {
            best = fix;
        }
    }

    // Old Androids call these; they are abstract there.
    @Override
    public void onStatusChanged(String provider, int status, Bundle extras) {}

    @Override
    public void onProviderEnabled(String provider) {}

    @Override
    public void onProviderDisabled(String provider) {}
}
