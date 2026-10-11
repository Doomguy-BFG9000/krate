package tech.krate.player;

import android.app.Activity;
import android.graphics.Bitmap;
import android.graphics.Outline;
import android.graphics.PixelFormat;
import android.net.http.SslError;
import android.util.Log;
import android.view.Gravity;
import android.view.View;
import android.view.ViewOutlineProvider;
import android.view.WindowManager;
import android.webkit.CookieManager;
import android.webkit.GeolocationPermissions;
import android.webkit.PermissionRequest;
import android.webkit.RenderProcessGoneDetail;
import android.webkit.SslErrorHandler;
import android.webkit.WebChromeClient;
import android.webkit.WebResourceError;
import android.webkit.WebResourceRequest;
import android.webkit.WebSettings;
import android.webkit.WebStorage;
import android.webkit.WebView;
import android.webkit.WebViewClient;
import java.util.ArrayList;
import java.util.HashMap;

/**
 * A web page inside an app (krate:ui/web-page, IC-927): a WebView in a panel
 * window over the app, at the box the app laid out.
 *
 * Every navigation -- a link, a redirect, a script's own -- is judged by
 * Krate's one rule (krate_adapter_common::net::web_page_allowed, reached
 * through {@link #allowed}) before it goes anywhere, and the start of every
 * page is judged again, so nothing reaches a host the app was not granted.
 * The page keeps nothing: cookies and storage start empty and are wiped,
 * with the cache, when it closes. Scripts run, but the page has no channel
 * to the app (no JavaScript interface) and is refused the camera, the
 * microphone, the location and files.
 *
 * NativeActivity gives its window's surface to native code, so a view in it
 * never draws; the page lives in a panel window attached to the activity's,
 * which also goes with the activity.
 */
final class Web {
    private Web() {}

    /** Null when the page may go to `address`, else why not (in Rust). */
    static native String allowed(long page, String address);

    private static final class Page {
        WebView view;
        WindowManager.LayoutParams where;
        String address = "";
        String title = "";
        boolean loading = true;
        String problem = "";
    }

    /** Pages by handle; every touch of a Page's fields holds this lock. */
    private static final HashMap<Long, Page> PAGES = new HashMap<>();

    private static Page get(long id) {
        synchronized (PAGES) {
            return PAGES.get(id);
        }
    }

    /** Ask Krate's rule; a missing native side refuses. */
    private static String judge(long id, String address) {
        if ("about:blank".equals(address)) {
            return null;
        }
        try {
            return allowed(id, address);
        } catch (UnsatisfiedLinkError e) {
            return "this player cannot check where a page goes";
        }
    }

    private static void say(long id, String problem) {
        synchronized (PAGES) {
            Page p = PAGES.get(id);
            if (p != null) {
                p.problem = problem;
            }
        }
    }

    private static WindowManager.LayoutParams at(Activity activity, int[] box) {
        WindowManager.LayoutParams lp = new WindowManager.LayoutParams(
                Math.max(1, box[2]), Math.max(1, box[3]),
                WindowManager.LayoutParams.TYPE_APPLICATION_PANEL,
                WindowManager.LayoutParams.FLAG_NOT_TOUCH_MODAL
                        | WindowManager.LayoutParams.FLAG_LAYOUT_NO_LIMITS
                        | WindowManager.LayoutParams.FLAG_LAYOUT_IN_SCREEN,
                PixelFormat.TRANSLUCENT);
        lp.gravity = Gravity.TOP | Gravity.LEFT;
        lp.x = box[0];
        lp.y = box[1];
        lp.token = activity.getWindow().getDecorView().getWindowToken();
        if (android.os.Build.VERSION.SDK_INT >= 28) {
            lp.layoutInDisplayCutoutMode =
                    WindowManager.LayoutParams.LAYOUT_IN_DISPLAY_CUTOUT_MODE_SHORT_EDGES;
        }
        lp.setTitle("Krate web page");
        return lp;
    }

    /** Wipe every cookie and every page's storage. */
    private static void forget() {
        CookieManager cookies = CookieManager.getInstance();
        cookies.removeAllCookies(null);
        cookies.removeSessionCookies(null);
        cookies.flush();
        WebStorage.getInstance().deleteAllData();
    }

    /**
     * Show `address` at `box` (x, y, width, height in pixels of the
     * screen) as page `id`. Returns at once; what goes wrong is said in the
     * page's problem.
     */
    static void open(final Activity activity, final long id, final String address,
            final int[] box) {
        final Page p = new Page();
        p.address = address;
        synchronized (PAGES) {
            PAGES.put(id, p);
        }
        activity.runOnUiThread(new Runnable() {
            @Override
            public void run() {
                try {
                    make(activity, id, p, address, box);
                } catch (RuntimeException e) {
                    Log.w(KrateActivity.TAG, "web page: could not open", e);
                    say(id, "the web view could not open: " + e.getMessage());
                }
            }
        });
    }

    private static void make(final Activity activity, final long id, final Page p,
            String address, int[] box) {
        boolean first;
        synchronized (PAGES) {
            first = PAGES.size() == 1;
        }
        // A page starts knowing nothing: no cookie or storage from an
        // earlier page or app. (Pages open together share the process's one
        // cookie jar, as the iPhone's pages of one app do.)
        if (first) {
            forget();
        }
        final WebView view = new WebView(activity);
        WebSettings s = view.getSettings();
        s.setJavaScriptEnabled(true);
        s.setDomStorageEnabled(true);
        s.setAllowFileAccess(false);
        s.setAllowContentAccess(false);
        s.setGeolocationEnabled(false);
        s.setSupportMultipleWindows(false);
        s.setJavaScriptCanOpenWindowsAutomatically(false);
        s.setMixedContentMode(WebSettings.MIXED_CONTENT_NEVER_ALLOW);
        s.setSaveFormData(false);
        CookieManager.getInstance().setAcceptCookie(true);
        CookieManager.getInstance().setAcceptThirdPartyCookies(view, false);
        view.setWebViewClient(new WebViewClient() {
            @Override
            public boolean shouldOverrideUrlLoading(WebView v, WebResourceRequest request) {
                // A link, a form, a script's location change, and (on
                // Android 7 and later) a redirect: judged before it goes.
                String to = request.getUrl().toString();
                String why = judge(id, to);
                if (why == null) {
                    if (request.isForMainFrame() && !request.isRedirect()) {
                        say(id, "");
                    }
                    return false;
                }
                Log.i(KrateActivity.TAG, "web page: refused " + to + " -- " + why);
                say(id, why);
                return true;
            }

            @Override
            public void onPageStarted(WebView v, String url, Bitmap favicon) {
                // The second check: any page that starts somewhere it may
                // not be (a redirect the first check never saw) is stopped
                // before it shows.
                String why = judge(id, url);
                if (why != null) {
                    Log.i(KrateActivity.TAG, "web page: stopped " + url + " -- " + why);
                    v.stopLoading();
                    say(id, why);
                    v.loadUrl("about:blank");
                    return;
                }
                synchronized (PAGES) {
                    p.loading = true;
                    if (!"about:blank".equals(url)) {
                        p.address = url;
                    }
                }
            }

            @Override
            public void onPageFinished(WebView v, String url) {
                synchronized (PAGES) {
                    p.loading = false;
                    if (!"about:blank".equals(url)) {
                        p.address = url;
                        String title = v.getTitle();
                        p.title = title == null ? "" : title;
                    }
                }
            }

            @Override
            public void onReceivedError(WebView v, WebResourceRequest request,
                    WebResourceError error) {
                if (!request.isForMainFrame()) {
                    return;
                }
                // A load Krate stopped is not a failure to say.
                if (error.getErrorCode() == WebViewClient.ERROR_UNKNOWN
                        && String.valueOf(error.getDescription()).contains("ERR_ABORTED")) {
                    return;
                }
                say(id, String.valueOf(error.getDescription()));
            }

            @Override
            public void onReceivedSslError(WebView v, SslErrorHandler handler, SslError error) {
                handler.cancel();
                say(id, "the page's certificate is not one this phone trusts");
            }

            @Override
            public boolean onRenderProcessGone(WebView v, RenderProcessGoneDetail detail) {
                // The page's process went; the app does not go with it.
                say(id, "the page stopped");
                remove(activity, id);
                return true;
            }
        });
        view.setWebChromeClient(new WebChromeClient() {
            @Override
            public void onReceivedTitle(WebView v, String title) {
                synchronized (PAGES) {
                    p.title = title == null ? "" : title;
                }
            }

            @Override
            public void onProgressChanged(WebView v, int progress) {
                synchronized (PAGES) {
                    p.loading = progress < 100;
                }
            }

            @Override
            public void onPermissionRequest(PermissionRequest request) {
                request.deny();
            }

            @Override
            public void onGeolocationPermissionsShowPrompt(String origin,
                    GeolocationPermissions.Callback callback) {
                callback.invoke(origin, false, false);
            }
        });
        final float radius = 12f * activity.getResources().getDisplayMetrics().density;
        view.setOutlineProvider(new ViewOutlineProvider() {
            @Override
            public void getOutline(View v, Outline outline) {
                outline.setRoundRect(0, 0, v.getWidth(), v.getHeight(), radius);
            }
        });
        view.setClipToOutline(true);
        WindowManager.LayoutParams where = at(activity, box);
        activity.getWindowManager().addView(view, where);
        synchronized (PAGES) {
            p.view = view;
            p.where = where;
        }
        String why = judge(id, address);
        if (why != null) {
            say(id, why);
            return;
        }
        view.loadUrl(address);
    }

    /** Move or resize page `id`'s box. */
    static void place(final Activity activity, final long id, final int[] box) {
        activity.runOnUiThread(new Runnable() {
            @Override
            public void run() {
                Page p = get(id);
                if (p == null || p.view == null) {
                    return;
                }
                p.where.x = box[0];
                p.where.y = box[1];
                p.where.width = Math.max(1, box[2]);
                p.where.height = Math.max(1, box[3]);
                activity.getWindowManager().updateViewLayout(p.view, p.where);
            }
        });
    }

    /** Send page `id` to `address`, which Krate already judged. */
    static void go(final Activity activity, final long id, final String address) {
        activity.runOnUiThread(new Runnable() {
            @Override
            public void run() {
                Page p = get(id);
                if (p == null || p.view == null) {
                    return;
                }
                String why = judge(id, address);
                if (why != null) {
                    say(id, why);
                    return;
                }
                say(id, "");
                p.view.loadUrl(address);
            }
        });
    }

    static void back(final Activity activity, final long id) {
        activity.runOnUiThread(new Runnable() {
            @Override
            public void run() {
                Page p = get(id);
                if (p != null && p.view != null && p.view.canGoBack()) {
                    p.view.goBack();
                }
            }
        });
    }

    /** {address, title, "1" when loading else "0", problem}; null if gone. */
    static String[] state(long id) {
        synchronized (PAGES) {
            Page p = PAGES.get(id);
            if (p == null) {
                return null;
            }
            return new String[] {p.address, p.title, p.loading ? "1" : "0", p.problem};
        }
    }

    /** Take page `id` away, and with the last page everything it kept. */
    static void close(final Activity activity, final long id) {
        activity.runOnUiThread(new Runnable() {
            @Override
            public void run() {
                remove(activity, id);
            }
        });
    }

    /** The app is gone: every page goes with it. */
    static void closeAll(final Activity activity) {
        activity.runOnUiThread(new Runnable() {
            @Override
            public void run() {
                ArrayList<Long> ids;
                synchronized (PAGES) {
                    ids = new ArrayList<>(PAGES.keySet());
                }
                for (Long id : ids) {
                    remove(activity, id);
                }
            }
        });
    }

    /** UI thread. */
    private static void remove(Activity activity, long id) {
        Page p;
        boolean last;
        synchronized (PAGES) {
            p = PAGES.remove(id);
            last = PAGES.isEmpty();
        }
        if (p == null) {
            return;
        }
        if (p.view != null) {
            WebView view = p.view;
            view.stopLoading();
            try {
                activity.getWindowManager().removeViewImmediate(view);
            } catch (IllegalArgumentException e) {
                // Already gone with the activity's window.
            }
            view.clearHistory();
            view.clearFormData();
            view.clearCache(true);
            view.destroy();
        }
        if (last) {
            forget();
        }
    }
}
