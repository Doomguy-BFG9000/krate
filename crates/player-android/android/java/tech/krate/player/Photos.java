package tech.krate.player;

import android.content.ContentResolver;
import android.content.ContentValues;
import android.content.Context;
import android.graphics.Bitmap;
import android.graphics.ImageDecoder;
import android.net.Uri;
import android.os.Build;
import android.provider.MediaStore;
import android.util.Log;

import java.io.File;
import java.io.FileOutputStream;
import java.io.IOException;
import java.io.OutputStream;

/**
 * The photo library (krate:ui/photos, IC-920). Picking is the system's photo
 * picker (KrateActivity starts it; it runs outside Krate, so it needs no
 * permission), each pick copied in -- a HEIC as a JPEG, as on iOS. Saving
 * adds a PNG or JPEG to Pictures/Krate through MediaStore, which needs no
 * permission from Android 10 on.
 */
final class Photos {
    private Photos() {}

    /** Copy a picked photo into dir: the path, or null. */
    static String copyIn(android.app.Activity context, Uri uri, String dir) {
        ContentResolver resolver = context.getContentResolver();
        String mime = resolver.getType(uri);
        boolean heif = mime != null && (mime.contains("heic") || mime.contains("heif"));
        if (heif && Build.VERSION.SDK_INT >= 28) {
            try {
                Bitmap picture = ImageDecoder.decodeBitmap(ImageDecoder.createSource(resolver, uri));
                File folder = new File(dir);
                if (!folder.isDirectory() && !folder.mkdirs()) {
                    return null;
                }
                File out = new File(folder, "Photo-" + System.currentTimeMillis() + ".jpg");
                try (OutputStream os = new FileOutputStream(out)) {
                    picture.compress(Bitmap.CompressFormat.JPEG, 92, os);
                }
                return out.getAbsolutePath();
            } catch (IOException | RuntimeException e) {
                Log.w(KrateActivity.TAG, "photos: could not turn a HEIC to JPEG: " + e);
            }
        }
        return Documents.copyIn(context, uri, dir);
    }

    /** Add a PNG or JPEG to the library: null once added, else why not. */
    static String save(Context context, byte[] data) {
        boolean png = data.length > 4 && (data[0] & 0xff) == 0x89 && data[1] == 'P';
        String mime = png ? "image/png" : "image/jpeg";
        String stamp = new java.text.SimpleDateFormat("yyyy-MM-dd 'at' HH.mm.ss",
                java.util.Locale.ROOT).format(new java.util.Date());
        String name = "Krate " + stamp + (png ? ".png" : ".jpg");
        ContentResolver resolver = context.getContentResolver();
        ContentValues values = new ContentValues();
        values.put(MediaStore.Images.Media.DISPLAY_NAME, name);
        values.put(MediaStore.Images.Media.MIME_TYPE, mime);
        Uri collection;
        if (Build.VERSION.SDK_INT >= 29) {
            values.put(MediaStore.Images.Media.RELATIVE_PATH, "Pictures/Krate");
            values.put(MediaStore.Images.Media.IS_PENDING, 1);
            collection = MediaStore.Images.Media.getContentUri(MediaStore.VOLUME_EXTERNAL_PRIMARY);
        } else {
            collection = MediaStore.Images.Media.EXTERNAL_CONTENT_URI;
        }
        Uri item;
        try {
            item = resolver.insert(collection, values);
        } catch (RuntimeException e) {
            return "the photo library refused it: " + e.getMessage();
        }
        if (item == null) {
            return "the photo library refused it";
        }
        try (OutputStream os = resolver.openOutputStream(item)) {
            if (os == null) {
                resolver.delete(item, null, null);
                return "the photo library would not take it";
            }
            os.write(data);
        } catch (IOException | RuntimeException e) {
            resolver.delete(item, null, null);
            return "the picture could not be written: " + e.getMessage();
        }
        if (Build.VERSION.SDK_INT >= 29) {
            ContentValues done = new ContentValues();
            done.put(MediaStore.Images.Media.IS_PENDING, 0);
            resolver.update(item, done, null, null);
        }
        Log.i(KrateActivity.TAG, "photos: added " + name);
        return null;
    }
}
