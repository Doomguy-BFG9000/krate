package tech.krate.player;

import android.content.ContentProvider;
import android.content.ContentValues;
import android.database.Cursor;
import android.database.MatrixCursor;
import android.net.Uri;
import android.os.ParcelFileDescriptor;
import android.provider.OpenableColumns;

import java.io.File;
import java.io.FileNotFoundException;
import java.io.IOException;

/**
 * Krate's own FileProvider, without AndroidX: it hands the app a share sheet
 * picked files in Krate's share folder (cache/krate-share), read-only, to the
 * one app the person sent them to -- and nothing else. A URI is
 * content://tech.krate.player.share/<folder>/<name>.
 */
public final class Shared extends ContentProvider {
    static final String AUTHORITY = "tech.krate.player.share";

    static Uri uriFor(File file) {
        File folder = file.getParentFile();
        return new Uri.Builder().scheme("content").authority(AUTHORITY)
                .appendPath(folder == null ? "" : folder.getName())
                .appendPath(file.getName()).build();
    }

    private File fileFor(Uri uri) throws FileNotFoundException {
        if (getContext() == null || uri.getPathSegments().size() != 2) {
            throw new FileNotFoundException(String.valueOf(uri));
        }
        File root = new File(getContext().getCacheDir(), "krate-share");
        String folder = uri.getPathSegments().get(0);
        String name = uri.getPathSegments().get(1);
        if (folder.contains("/") || name.contains("/") || folder.startsWith(".")
                || name.startsWith(".")) {
            throw new FileNotFoundException(String.valueOf(uri));
        }
        File file = new File(new File(root, folder), name);
        try {
            if (!file.getCanonicalPath().startsWith(root.getCanonicalPath() + File.separator)) {
                throw new FileNotFoundException(String.valueOf(uri));
            }
        } catch (IOException e) {
            throw new FileNotFoundException(String.valueOf(uri));
        }
        return file;
    }

    @Override
    public boolean onCreate() {
        return true;
    }

    @Override
    public ParcelFileDescriptor openFile(Uri uri, String mode) throws FileNotFoundException {
        if (!"r".equals(mode)) {
            throw new FileNotFoundException("shared files are read-only");
        }
        return ParcelFileDescriptor.open(fileFor(uri), ParcelFileDescriptor.MODE_READ_ONLY);
    }

    @Override
    public Cursor query(Uri uri, String[] projection, String selection, String[] args,
            String order) {
        File file;
        try {
            file = fileFor(uri);
        } catch (FileNotFoundException e) {
            return null;
        }
        String[] columns = projection != null ? projection
                : new String[] {OpenableColumns.DISPLAY_NAME, OpenableColumns.SIZE};
        MatrixCursor cursor = new MatrixCursor(columns, 1);
        Object[] row = new Object[columns.length];
        for (int i = 0; i < columns.length; i++) {
            if (OpenableColumns.DISPLAY_NAME.equals(columns[i])) {
                row[i] = file.getName();
            } else if (OpenableColumns.SIZE.equals(columns[i])) {
                row[i] = file.length();
            }
        }
        cursor.addRow(row);
        return cursor;
    }

    @Override
    public String getType(Uri uri) {
        return Documents.mimeOf(uri.getLastPathSegment() == null ? "" : uri.getLastPathSegment());
    }

    @Override
    public Uri insert(Uri uri, ContentValues values) {
        throw new UnsupportedOperationException("read-only");
    }

    @Override
    public int delete(Uri uri, String selection, String[] args) {
        throw new UnsupportedOperationException("read-only");
    }

    @Override
    public int update(Uri uri, ContentValues values, String selection, String[] args) {
        throw new UnsupportedOperationException("read-only");
    }
}
