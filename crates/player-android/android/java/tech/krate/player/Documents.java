package tech.krate.player;

import android.app.Activity;
import android.content.ContentResolver;
import android.database.Cursor;
import android.net.Uri;
import android.provider.DocumentsContract;
import android.provider.OpenableColumns;
import android.util.Log;
import android.webkit.MimeTypeMap;
import android.widget.Toast;

import java.io.File;
import java.io.FileInputStream;
import java.io.FileOutputStream;
import java.io.IOException;
import java.io.InputStream;
import java.io.OutputStream;
import java.util.ArrayList;
import java.util.Collections;
import java.util.HashMap;
import java.util.Map;

/**
 * Files and folders the person picks in the system's picker, made into
 * ordinary files Krate's runtime can open (K-1032).
 *
 * Android hands a pick over as a content URI, never a path, and the runtime
 * works with paths. So a picked file is copied in, as iOS does; a picked
 * folder is copied in whole, and what the app changes in the copy -- a file
 * written, renamed, removed -- is put back in the real folder when the app
 * closes. A save is written in Krate first and then copied out to the place
 * the person picks. All of it runs on Krate's native thread, never the UI
 * thread.
 */
final class Documents {
    private Documents() {}

    /** A folder copied in: where it came from, and each file as it was copied. */
    private static final class Mirror {
        Uri tree;
        /** Folder path below the copy's root ("" is the root) to its document id. */
        final HashMap<String, String> dirs = new HashMap<>();
        /** File path below the root to its document id, size and time as copied. */
        final HashMap<String, Entry> files = new HashMap<>();
    }

    private static final class Entry {
        String id;
        long size;
        long modified;
    }

    /** Folders copied in this run, by the copy's root path. */
    private static final HashMap<String, Mirror> MIRRORS = new HashMap<>();

    static String copyIn(Activity activity, Uri uri, String dir) {
        ContentResolver resolver = activity.getContentResolver();
        String name = safeName(displayName(resolver, uri, OpenableColumns.DISPLAY_NAME));
        File folder = new File(dir);
        if (!folder.isDirectory() && !folder.mkdirs()) {
            Log.w(KrateActivity.TAG, "could not make " + dir);
            return null;
        }
        File out = new File(folder, name);
        try (InputStream in = resolver.openInputStream(uri);
             OutputStream os = new FileOutputStream(out)) {
            if (in == null) {
                return null;
            }
            copy(in, os);
            return out.getAbsolutePath();
        } catch (IOException | RuntimeException e) {
            Log.w(KrateActivity.TAG, "could not copy the picked file: " + e);
            return null;
        }
    }

    static boolean copyOut(Activity activity, String path, Uri uri) {
        try (InputStream in = new FileInputStream(path);
             OutputStream os = activity.getContentResolver().openOutputStream(uri, "wt")) {
            if (os == null) {
                return false;
            }
            copy(in, os);
            return true;
        } catch (IOException | RuntimeException e) {
            Log.w(KrateActivity.TAG, "could not save to the picked place: " + e);
            return false;
        }
    }

    static String mirror(Activity activity, Uri tree, String dir, long maxBytes) {
        ContentResolver resolver = activity.getContentResolver();
        String rootId;
        try {
            rootId = DocumentsContract.getTreeDocumentId(tree);
        } catch (RuntimeException e) {
            Log.w(KrateActivity.TAG, "not a folder: " + e);
            return null;
        }
        Uri rootDoc = DocumentsContract.buildDocumentUriUsingTree(tree, rootId);
        String rootName = safeName(displayName(resolver, rootDoc,
                DocumentsContract.Document.COLUMN_DISPLAY_NAME));
        File root = new File(dir, rootName);
        if (!root.isDirectory() && !root.mkdirs()) {
            Log.w(KrateActivity.TAG, "could not make " + root);
            return null;
        }
        Mirror mirror = new Mirror();
        mirror.tree = tree;
        mirror.dirs.put("", rootId);
        long[] total = new long[] {0};
        try {
            copyTree(resolver, tree, rootId, root, "", mirror, total, maxBytes);
        } catch (TooBig e) {
            deleteAll(root);
            final long megabytes = maxBytes / (1024 * 1024);
            activity.runOnUiThread(new Runnable() {
                @Override
                public void run() {
                    Toast.makeText(activity, "That folder holds more than " + megabytes
                            + " MB, too much to open here. Pick a smaller one.",
                            Toast.LENGTH_LONG).show();
                }
            });
            return null;
        } catch (IOException | RuntimeException e) {
            Log.w(KrateActivity.TAG, "could not copy the picked folder: " + e);
            deleteAll(root);
            return null;
        }
        synchronized (MIRRORS) {
            MIRRORS.put(root.getAbsolutePath(), mirror);
        }
        return root.getAbsolutePath();
    }

    private static final class TooBig extends Exception {
        private static final long serialVersionUID = 1L;
    }

    private static void copyTree(ContentResolver resolver, Uri tree, String parentId, File into,
            String rel, Mirror mirror, long[] total, long maxBytes) throws IOException, TooBig {
        Uri children = DocumentsContract.buildChildDocumentsUriUsingTree(tree, parentId);
        String[] columns = {
            DocumentsContract.Document.COLUMN_DOCUMENT_ID,
            DocumentsContract.Document.COLUMN_DISPLAY_NAME,
            DocumentsContract.Document.COLUMN_MIME_TYPE,
            DocumentsContract.Document.COLUMN_SIZE,
        };
        ArrayList<String[]> found = new ArrayList<>();
        try (Cursor c = resolver.query(children, columns, null, null, null)) {
            if (c == null) {
                return;
            }
            while (c.moveToNext()) {
                found.add(new String[] {c.getString(0), c.getString(1), c.getString(2),
                        c.isNull(3) ? "0" : Long.toString(c.getLong(3))});
            }
        }
        for (String[] child : found) {
            String id = child[0];
            String name = safeName(child[1]);
            String childRel = rel.isEmpty() ? name : rel + "/" + name;
            File local = new File(into, name);
            if (DocumentsContract.Document.MIME_TYPE_DIR.equals(child[2])) {
                if (!local.isDirectory() && !local.mkdirs()) {
                    throw new IOException("could not make " + local);
                }
                mirror.dirs.put(childRel, id);
                copyTree(resolver, tree, id, local, childRel, mirror, total, maxBytes);
                continue;
            }
            total[0] += Long.parseLong(child[3]);
            if (total[0] > maxBytes) {
                throw new TooBig();
            }
            Uri doc = DocumentsContract.buildDocumentUriUsingTree(tree, id);
            try (InputStream in = resolver.openInputStream(doc);
                 OutputStream os = new FileOutputStream(local)) {
                if (in != null) {
                    copy(in, os);
                }
            }
            Entry entry = new Entry();
            entry.id = id;
            entry.size = local.length();
            entry.modified = local.lastModified();
            mirror.files.put(childRel, entry);
        }
    }

    /**
     * Put back what changed in a mirrored folder: new and changed files are
     * written into the real folder, files and folders the app removed are
     * removed there. Returns how many things changed, or -1 when root is no
     * copy of this run's.
     */
    static int syncBack(Activity activity, String root) {
        Mirror mirror;
        synchronized (MIRRORS) {
            mirror = MIRRORS.get(root);
        }
        if (mirror == null) {
            return -1;
        }
        ContentResolver resolver = activity.getContentResolver();
        ArrayList<String> localDirs = new ArrayList<>();
        ArrayList<String> localFiles = new ArrayList<>();
        walk(new File(root), "", localDirs, localFiles);
        int changed = 0;
        // Folders first, parents before children (the walk lists them so).
        for (String rel : localDirs) {
            if (mirror.dirs.containsKey(rel)) {
                continue;
            }
            String parentId = mirror.dirs.get(parentOf(rel));
            if (parentId == null) {
                continue;
            }
            try {
                Uri made = DocumentsContract.createDocument(resolver,
                        DocumentsContract.buildDocumentUriUsingTree(mirror.tree, parentId),
                        DocumentsContract.Document.MIME_TYPE_DIR, nameOf(rel));
                if (made != null) {
                    mirror.dirs.put(rel, DocumentsContract.getDocumentId(made));
                    changed++;
                }
            } catch (IOException | RuntimeException e) {
                Log.w(KrateActivity.TAG, "could not make folder " + rel + ": " + e);
            }
        }
        for (String rel : localFiles) {
            File local = new File(root, rel);
            Entry entry = mirror.files.get(rel);
            if (entry != null && entry.size == local.length()
                    && entry.modified == local.lastModified()) {
                continue;
            }
            try {
                Uri doc;
                if (entry != null) {
                    doc = DocumentsContract.buildDocumentUriUsingTree(mirror.tree, entry.id);
                } else {
                    String parentId = mirror.dirs.get(parentOf(rel));
                    if (parentId == null) {
                        continue;
                    }
                    doc = DocumentsContract.createDocument(resolver,
                            DocumentsContract.buildDocumentUriUsingTree(mirror.tree, parentId),
                            mimeOf(rel), nameOf(rel));
                    if (doc == null) {
                        continue;
                    }
                    entry = new Entry();
                    entry.id = DocumentsContract.getDocumentId(doc);
                    mirror.files.put(rel, entry);
                }
                try (InputStream in = new FileInputStream(local);
                     OutputStream os = resolver.openOutputStream(doc, "wt")) {
                    if (os != null) {
                        copy(in, os);
                    }
                }
                entry.size = local.length();
                entry.modified = local.lastModified();
                changed++;
            } catch (IOException | RuntimeException e) {
                Log.w(KrateActivity.TAG, "could not put back " + rel + ": " + e);
            }
        }
        // What the app removed, files then folders deepest first.
        for (Map.Entry<String, Entry> was : new ArrayList<>(mirror.files.entrySet())) {
            if (new File(root, was.getKey()).exists()) {
                continue;
            }
            if (delete(resolver, mirror.tree, was.getValue().id)) {
                mirror.files.remove(was.getKey());
                changed++;
            }
        }
        ArrayList<String> gone = new ArrayList<>();
        for (String rel : mirror.dirs.keySet()) {
            if (!rel.isEmpty() && !new File(root, rel).isDirectory()) {
                gone.add(rel);
            }
        }
        Collections.sort(gone, Collections.reverseOrder());
        for (String rel : gone) {
            if (delete(resolver, mirror.tree, mirror.dirs.get(rel))) {
                mirror.dirs.remove(rel);
                changed++;
            }
        }
        return changed;
    }

    private static boolean delete(ContentResolver resolver, Uri tree, String id) {
        try {
            return DocumentsContract.deleteDocument(resolver,
                    DocumentsContract.buildDocumentUriUsingTree(tree, id));
        } catch (IOException | RuntimeException e) {
            // Already gone with its folder counts as done.
            return true;
        }
    }

    private static void walk(File dir, String rel, ArrayList<String> dirs, ArrayList<String> files) {
        File[] children = dir.listFiles();
        if (children == null) {
            return;
        }
        for (File child : children) {
            String childRel = rel.isEmpty() ? child.getName() : rel + "/" + child.getName();
            if (child.isDirectory()) {
                dirs.add(childRel);
                walk(child, childRel, dirs, files);
            } else if (child.isFile()) {
                files.add(childRel);
            }
        }
    }

    private static String parentOf(String rel) {
        int slash = rel.lastIndexOf('/');
        return slash < 0 ? "" : rel.substring(0, slash);
    }

    private static String nameOf(String rel) {
        return rel.substring(rel.lastIndexOf('/') + 1);
    }

    static String mimeOf(String name) {
        int dot = name.lastIndexOf('.');
        if (dot >= 0) {
            String mime = MimeTypeMap.getSingleton()
                    .getMimeTypeFromExtension(name.substring(dot + 1).toLowerCase());
            if (mime != null) {
                return mime;
            }
        }
        return "application/octet-stream";
    }

    private static String displayName(ContentResolver resolver, Uri uri, String column) {
        try (Cursor c = resolver.query(uri, new String[] {column}, null, null, null)) {
            if (c != null && c.moveToFirst() && !c.isNull(0)) {
                return c.getString(0);
            }
        } catch (RuntimeException e) {
            Log.w(KrateActivity.TAG, "no name for " + uri + ": " + e);
        }
        String last = uri.getLastPathSegment();
        return last == null ? "" : last.substring(last.lastIndexOf('/') + 1);
    }

    /** A name that stays one name: no slashes, never "." or "..". */
    static String safeName(String name) {
        if (name == null) {
            return "Untitled";
        }
        String clean = name.replace('/', '-').replace('\\', '-').replace('\0', '-').trim();
        if (clean.isEmpty() || clean.equals(".") || clean.equals("..")) {
            return "Untitled";
        }
        return clean;
    }

    private static void copy(InputStream in, OutputStream out) throws IOException {
        byte[] buffer = new byte[64 * 1024];
        int n;
        while ((n = in.read(buffer)) > 0) {
            out.write(buffer, 0, n);
        }
    }

    private static void deleteAll(File file) {
        File[] children = file.listFiles();
        if (children != null) {
            for (File child : children) {
                deleteAll(child);
            }
        }
        if (!file.delete()) {
            Log.w(KrateActivity.TAG, "could not remove " + file);
        }
    }
}
