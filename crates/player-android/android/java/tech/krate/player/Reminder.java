package tech.krate.player;

import android.app.AlarmManager;
import android.app.Notification;
import android.app.NotificationChannel;
import android.app.NotificationManager;
import android.app.PendingIntent;
import android.content.BroadcastReceiver;
import android.content.Context;
import android.content.Intent;
import android.content.SharedPreferences;
import android.net.Uri;
import android.os.Build;
import android.util.Log;

import java.util.ArrayList;
import java.util.Map;

/**
 * Reminders that arrive with the app closed (krate:ui/reminders, IC-921):
 * AlarmManager wakes this receiver, which posts the notification on the
 * "Reminders" channel; tapping it opens Krate on the app that set it. Each
 * is kept in Krate's preferences until it has come (a daily one is set for
 * the next day), so a restart of the phone sets them again.
 *
 * Android 12 and up give exact alarms only to an app the person allowed in
 * Settings ("Alarms & reminders"); without that a reminder still comes, at a
 * time Android picks close to the one asked for.
 */
public final class Reminder extends BroadcastReceiver {
    static final String CHANNEL = "krate-reminders";
    private static final String STORE = "krate-reminders";
    private static final String FIRE = "tech.krate.player.REMIND";
    private static final long DAY = 24L * 60 * 60 * 1000;
    /** Android's word that the person allowed Krate exact alarms. */
    private static final String EXACT_ALLOWED =
            "android.app.action.SCHEDULE_EXACT_ALARM_PERMISSION_STATE_CHANGED";
    /** Apps already asked about exact alarms, so each is asked once. */
    private static final String ASKED = "krate-exact-asked";

    /** Whether reminders can come at the time asked. */
    static boolean exactAllowed(Context context) {
        if (Build.VERSION.SDK_INT < 31) {
            return true;
        }
        AlarmManager alarms = (AlarmManager) context.getSystemService(Context.ALARM_SERVICE);
        return alarms != null && alarms.canScheduleExactAlarms();
    }

    /** Whether app was asked before; marks it asked. */
    static boolean askedBefore(Context context, String app) {
        SharedPreferences asked = context.getSharedPreferences(ASKED, Context.MODE_PRIVATE);
        if (asked.getBoolean(app, false)) {
            return true;
        }
        asked.edit().putBoolean(app, true).apply();
        return false;
    }

    /** One reminder, as stored: when (wall clock ms), daily, sender, title, body, opens. */
    private static String pack(long at, boolean daily, String sender, String title, String body,
            String opens) {
        return at + "\u001f" + (daily ? 1 : 0) + "\u001f" + sender + "\u001f" + title + "\u001f"
                + body + "\u001f" + opens;
    }

    private static PendingIntent fireIntent(Context context, String id) {
        Intent intent = new Intent(context, Reminder.class).setAction(FIRE)
                .setData(Uri.parse("krate-reminder:" + Uri.encode(id)));
        return PendingIntent.getBroadcast(context, id.hashCode(), intent,
                PendingIntent.FLAG_UPDATE_CURRENT | PendingIntent.FLAG_IMMUTABLE);
    }

    /** Set (or replace) reminder id; null once set, else why not. */
    static String schedule(Context context, String id, String sender, String title, String body,
            long afterSeconds, boolean daily, String opens) {
        long at = System.currentTimeMillis() + afterSeconds * 1000L;
        context.getSharedPreferences(STORE, Context.MODE_PRIVATE).edit()
                .putString(id, pack(at, daily, sender, title, body, opens)).apply();
        return arm(context, id, at);
    }

    private static String arm(Context context, String id, long at) {
        AlarmManager alarms = (AlarmManager) context.getSystemService(Context.ALARM_SERVICE);
        if (alarms == null) {
            return "this phone has no alarms";
        }
        PendingIntent fire = fireIntent(context, id);
        try {
            if (Build.VERSION.SDK_INT < 31 || alarms.canScheduleExactAlarms()) {
                alarms.setExactAndAllowWhileIdle(AlarmManager.RTC_WAKEUP, at, fire);
                Log.i(KrateActivity.TAG, "reminder " + id + ": set, exact");
            } else {
                alarms.setAndAllowWhileIdle(AlarmManager.RTC_WAKEUP, at, fire);
                Log.i(KrateActivity.TAG, "reminder " + id + ": set, near the time");
            }
            return null;
        } catch (SecurityException e) {
            alarms.setAndAllowWhileIdle(AlarmManager.RTC_WAKEUP, at, fire);
            return null;
        }
    }

    static void cancel(Context context, String id) {
        AlarmManager alarms = (AlarmManager) context.getSystemService(Context.ALARM_SERVICE);
        if (alarms != null) {
            alarms.cancel(fireIntent(context, id));
        }
        context.getSharedPreferences(STORE, Context.MODE_PRIVATE).edit().remove(id).apply();
        Log.i(KrateActivity.TAG, "reminder " + id + ": cancelled");
    }

    /** The ids still to come that start with prefix. */
    static String[] pending(Context context, String prefix) {
        ArrayList<String> out = new ArrayList<>();
        for (Map.Entry<String, ?> e :
                context.getSharedPreferences(STORE, Context.MODE_PRIVATE).getAll().entrySet()) {
            if (e.getKey().startsWith(prefix)) {
                out.add(e.getKey());
            }
        }
        return out.toArray(new String[0]);
    }

    @Override
    public void onReceive(Context context, Intent intent) {
        SharedPreferences store = context.getSharedPreferences(STORE, Context.MODE_PRIVATE);
        if (Intent.ACTION_BOOT_COMPLETED.equals(intent.getAction())
                || EXACT_ALLOWED.equals(intent.getAction())) {
            // A restarted phone forgets its alarms, and alarms set before
            // the person allowed exact ones were set near the time: set every
            // reminder again (exact now, where allowed).
            for (Map.Entry<String, ?> e : store.getAll().entrySet()) {
                String[] r = String.valueOf(e.getValue()).split("\u001f", -1);
                long at = Long.parseLong(r[0]);
                arm(context, e.getKey(), Math.max(at, System.currentTimeMillis() + 1000));
            }
            return;
        }
        if (!FIRE.equals(intent.getAction()) || intent.getData() == null) {
            return;
        }
        String id = Uri.decode(intent.getData().getSchemeSpecificPart());
        String packed = store.getString(id, null);
        if (packed == null) {
            return;
        }
        String[] r = packed.split("\u001f", -1);
        if (r.length < 6) {
            store.edit().remove(id).apply();
            return;
        }
        boolean daily = "1".equals(r[1]);
        post(context, id, r[2], r[3], r[4], r[5]);
        if (daily) {
            long next = Long.parseLong(r[0]) + DAY;
            while (next <= System.currentTimeMillis()) {
                next += DAY;
            }
            store.edit().putString(id, pack(next, true, r[2], r[3], r[4], r[5])).apply();
            arm(context, id, next);
        } else {
            store.edit().remove(id).apply();
        }
    }

    private static void post(Context context, String id, String sender, String title,
            String body, String opens) {
        NotificationManager manager =
                (NotificationManager) context.getSystemService(Context.NOTIFICATION_SERVICE);
        if (manager == null) {
            return;
        }
        if (manager.getNotificationChannel(CHANNEL) == null) {
            NotificationChannel channel = new NotificationChannel(CHANNEL, "Reminders",
                    NotificationManager.IMPORTANCE_HIGH);
            channel.setDescription("Reminders your apps set, which come with the app closed");
            manager.createNotificationChannel(channel);
        }
        // Tapping opens Krate on the app that set it: the copy of it Krate
        // kept, or a plain start for the app Krate carries.
        Intent open = new Intent(context, KrateActivity.class);
        if (opens != null && !opens.isEmpty()) {
            open.setAction(Intent.ACTION_VIEW);
            open.setDataAndType(Uri.parse(opens), "application/octet-stream");
        } else {
            open.setAction(Intent.ACTION_MAIN);
        }
        open.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK);
        PendingIntent tap = PendingIntent.getActivity(context, id.hashCode(), open,
                PendingIntent.FLAG_UPDATE_CURRENT | PendingIntent.FLAG_IMMUTABLE);
        Notification note = new Notification.Builder(context, CHANNEL)
                .setSmallIcon(android.R.drawable.ic_popup_reminder)
                .setContentTitle(title)
                .setContentText(body)
                .setStyle(new Notification.BigTextStyle().bigText(body))
                .setSubText(sender)
                .setContentIntent(tap)
                .setAutoCancel(true)
                .build();
        manager.notify(id.hashCode(), note);
        Log.i(KrateActivity.TAG, "reminder " + id + ": shown");
    }
}
