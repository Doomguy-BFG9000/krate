package tech.krate.player;

import android.app.Activity;
import android.content.Intent;
import android.os.Bundle;
import android.os.Process;

/**
 * Restart from Krate's menu (IC-934). Krate runs one app per process, and a
 * second player cannot start beside a running one, so Restart starts this,
 * in a process of its own (":restart"): it ends Krate's process, opens
 * Krate again on the same app, and ends itself.
 */
public final class Restarter extends Activity {
    static final String OPEN = "tech.krate.player.OPEN";
    static final String PID = "tech.krate.player.PID";

    @Override
    @SuppressWarnings("deprecation")
    protected void onCreate(Bundle saved) {
        super.onCreate(saved);
        int pid = getIntent().getIntExtra(PID, -1);
        if (pid > 0) {
            Process.killProcess(pid);
        }
        Intent open = getIntent().getParcelableExtra(OPEN);
        if (open != null) {
            open.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK);
            startActivity(open);
        }
        finish();
        Runtime.getRuntime().exit(0);
    }
}
