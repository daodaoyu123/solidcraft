package io.archipelagos.solidcraft;

import android.app.NativeActivity;
import android.content.ClipData;
import android.content.ClipboardManager;
import android.content.ContentResolver;
import android.content.Context;
import android.content.Intent;
import android.database.Cursor;
import android.net.Uri;
import android.os.Build;
import android.os.Bundle;
import android.provider.DocumentsContract;
import android.provider.OpenableColumns;
import android.security.keystore.KeyGenParameterSpec;
import android.security.keystore.KeyProperties;
import android.util.Base64;
import android.util.Log;
import android.text.Editable;
import android.text.InputType;
import android.text.Selection;
import android.view.KeyEvent;
import android.view.View;
import android.view.ViewGroup;
import android.view.WindowInsets;
import android.view.inputmethod.BaseInputConnection;
import android.view.inputmethod.EditorInfo;
import android.view.inputmethod.InputConnection;
import android.view.inputmethod.InputMethodManager;
import android.webkit.MimeTypeMap;
import android.widget.Toast;
import android.app.AlertDialog;
import android.widget.EditText;
import android.widget.LinearLayout;
import android.widget.ScrollView;
import android.widget.TextView;

import org.json.JSONObject;

import java.io.File;
import java.io.FileInputStream;
import java.io.FileOutputStream;
import java.io.InputStream;
import java.io.OutputStream;
import java.nio.charset.StandardCharsets;
import java.util.HashMap;

/**
 * SolidCraft's Android activity: the Rust app runs in NativeActivity's thread; this class adds
 * what needs Java: the Storage Access Framework pickers (Open, Save, Save As, Import, Export),
 * copying documents in and out of content:// URIs, the clipboard, the soft keyboard and files
 * opened from other apps. The Rust side works on ordinary files in the app's storage; a picked
 * document is a local copy linked to its URI, written back when the app saves it.
 */
public class MainActivity extends NativeActivity {
    private static final String TAG = "SolidCraft";

    static {
        // Load before NativeActivity does, through the class loader, so the native methods below
        // resolve to the library's Java_* exports.
        System.loadLibrary("solidcraft_android");
    }

    /** A picker answered: the local copy (or the file to write) and its document; nulls when cancelled. */
    /** Hands native code this activity (ndk-context only carries the Application). */
    native void nativeAttach();

    static native void nativePicked(int id, String local, String uri);

    /** SMB add/edit network location: `json` saved fields, or null when cancelled. */
    static native void nativeSmbConn(int token, String json);

    /** A document opened from another app (Files, a browser download): its local copy. */
    static native void nativeIncoming(String local, String uri);

    /** How much of the window's bottom the soft keyboard covers (px; 0 when hidden). */
    static native void nativeImeInset(int px);

    /** The soft keyboard's text, for the field egui is editing: 0 commit, 1 composing (pre-edit), 2 delete around the cursor. */
    static native void nativeImeText(int kind, String text, int before, int after);

    /**
     * What the keyboard types into. NativeActivity gives a keyboard only key events, so swipe,
     * suggestions, autocorrect and composing (CJK) need a text editor view: this one keeps the
     * session's text in an Editable for the keyboard to read, and forwards every change to egui
     * as IME events. Its own text is reset each time a field gains focus.
     */
    static final class ImeView extends View {
        ImeView(Context c) {
            super(c);
            setFocusable(true);
            setFocusableInTouchMode(true);
        }

        @Override
        public boolean onCheckIsTextEditor() {
            return true;
        }

        /** The field being typed in is a password: the keyboard hides it, suggests and learns nothing. */
        volatile boolean password;

        @Override
        public InputConnection onCreateInputConnection(EditorInfo out) {
            out.inputType = password
                ? InputType.TYPE_CLASS_TEXT | InputType.TYPE_TEXT_VARIATION_PASSWORD
                : InputType.TYPE_CLASS_TEXT | InputType.TYPE_TEXT_FLAG_AUTO_CORRECT;
            out.imeOptions = EditorInfo.IME_FLAG_NO_FULLSCREEN | EditorInfo.IME_FLAG_NO_EXTRACT_UI | EditorInfo.IME_ACTION_DONE
                | (password ? EditorInfo.IME_FLAG_NO_PERSONALIZED_LEARNING : 0);
            out.initialSelStart = 0;
            out.initialSelEnd = 0;
            return new Connection(this);
        }
    }

    static final class Connection extends BaseInputConnection {
        private String composing = "";

        Connection(View v) {
            super(v, true);
        }

        @Override
        public boolean commitText(CharSequence text, int newCursorPosition) {
            composing = "";
            nativeImeText(0, text.toString(), 0, 0);
            return super.commitText(text, newCursorPosition);
        }

        @Override
        public boolean setComposingText(CharSequence text, int newCursorPosition) {
            composing = text.toString();
            nativeImeText(1, composing, 0, 0);
            return super.setComposingText(text, newCursorPosition);
        }

        @Override
        public boolean setComposingRegion(int start, int end) {
            // A word typed earlier becomes the composition again (autocorrect, a suggestion).
            Editable e = getEditable();
            if (e != null) {
                int a = Math.max(0, Math.min(start, end)), b = Math.min(e.length(), Math.max(start, end));
                int cursor = Selection.getSelectionEnd(e);
                if (a < b && cursor >= a && cursor <= b) {
                    composing = e.subSequence(a, b).toString();
                    nativeImeText(2, null, cursor - a, b - cursor);
                    nativeImeText(1, composing, 0, 0);
                }
            }
            return super.setComposingRegion(start, end);
        }

        @Override
        public boolean finishComposingText() {
            if (!composing.isEmpty()) nativeImeText(0, composing, 0, 0);
            composing = "";
            return super.finishComposingText();
        }

        @Override
        public boolean deleteSurroundingText(int before, int after) {
            Editable e = getEditable();
            if (e != null) {
                int cursor = Selection.getSelectionEnd(e);
                int b = Math.min(before, Math.max(0, cursor)), a = Math.min(after, Math.max(0, e.length() - cursor));
                if (b > 0 || a > 0) nativeImeText(2, null, b, a);
                if (b < before) {
                    // More than this session typed: the rest is the field's own text.
                    for (int i = b; i < before; i++) {
                        sendKeyEvent(new KeyEvent(KeyEvent.ACTION_DOWN, KeyEvent.KEYCODE_DEL));
                        sendKeyEvent(new KeyEvent(KeyEvent.ACTION_UP, KeyEvent.KEYCODE_DEL));
                    }
                }
            }
            return super.deleteSurroundingText(before, after);
        }
    }

    private ImeView ime;

    private final HashMap<Integer, Object[]> pending = new HashMap<>();
    private int nextRequest = 1;

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        nativeAttach();
        super.onCreate(savedInstanceState);
        // The window keeps its size under the keyboard (the native surface can't be resized
        // for it); the app moves what is being typed into above it instead.
        getWindow().getDecorView().setOnApplyWindowInsetsListener((v, insets) -> {
            int bottom = Build.VERSION.SDK_INT >= 30
                ? insets.getInsets(WindowInsets.Type.ime()).bottom
                : insets.getSystemWindowInsetBottom();
            nativeImeInset(bottom);
            return v.onApplyWindowInsets(insets);
        });
        ime = new ImeView(this);
        addContentView(ime, new ViewGroup.LayoutParams(1, 1));
        handleView(getIntent());
    }

    /** Called from Rust: open a link (help, documentation, issue report) in the browser. */
    public void openUrl(final String url) {
        runOnUiThread(() -> {
            try {
                startActivity(new Intent(Intent.ACTION_VIEW, Uri.parse(url)));
            } catch (Exception e) {
                Log.e(TAG, "no app opens " + url, e);
            }
        });
    }

    @Override
    protected void onNewIntent(Intent intent) {
        super.onNewIntent(intent);
        setIntent(intent);
        handleView(intent);
    }

    private void handleView(Intent intent) {
        if (intent == null || !Intent.ACTION_VIEW.equals(intent.getAction()) || intent.getData() == null) return;
        Uri uri = intent.getData();
        try {
            try {
                getContentResolver().takePersistableUriPermission(uri, Intent.FLAG_GRANT_READ_URI_PERMISSION | Intent.FLAG_GRANT_WRITE_URI_PERMISSION);
            } catch (Exception ignored) {
                // Not every provider grants lasting access; the copy still opens.
            }
            String local = copyIn(uri, null);
            nativeIncoming(local, uri.toString());
        } catch (Exception e) {
            Log.e(TAG, "open from another app failed", e);
        }
    }

    /** Called from Rust: ask for a document to open. */
    public void pickOpen(final int id) {
        runOnUiThread(() -> {
            Intent i = new Intent(Intent.ACTION_OPEN_DOCUMENT);
            i.addCategory(Intent.CATEGORY_OPENABLE);
            i.setType("*/*");
            i.addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION | Intent.FLAG_GRANT_WRITE_URI_PERMISSION | Intent.FLAG_GRANT_PERSISTABLE_URI_PERMISSION);
            start(i, id, null);
        });
    }

    /** Called from Rust: ask where to save a document named `name`. */
    public void pickSave(final int id, final String name) {
        runOnUiThread(() -> {
            if (name.toLowerCase().endsWith(".urdf")) {
                // A URDF is the robot description plus its meshes beside it: ask for a folder.
                Intent t = new Intent(Intent.ACTION_OPEN_DOCUMENT_TREE);
                t.addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION | Intent.FLAG_GRANT_WRITE_URI_PERMISSION | Intent.FLAG_GRANT_PERSISTABLE_URI_PERMISSION);
                start(t, id, name);
                return;
            }
            Intent i = new Intent(Intent.ACTION_CREATE_DOCUMENT);
            i.addCategory(Intent.CATEGORY_OPENABLE);
            i.setType(mimeFor(name));
            i.putExtra(Intent.EXTRA_TITLE, name);
            start(i, id, name);
        });
    }

    private void start(Intent i, int id, String saveName) {
        int request = nextRequest++ & 0x7fff;
        pending.put(request, new Object[] {id, saveName});
        try {
            startActivityForResult(i, request);
        } catch (Exception e) {
            Log.e(TAG, "no document picker", e);
            pending.remove(request);
            nativePicked(id, null, null);
        }
    }

    @Override
    protected void onActivityResult(int requestCode, int resultCode, Intent data) {
        super.onActivityResult(requestCode, resultCode, data);
        Object[] p = pending.remove(requestCode);
        if (p == null) return;
        int id = (Integer) p[0];
        String saveName = (String) p[1];
        Uri uri = data == null ? null : data.getData();
        if (resultCode != RESULT_OK || uri == null) {
            nativePicked(id, null, null);
            return;
        }
        try {
            try {
                getContentResolver().takePersistableUriPermission(uri, Intent.FLAG_GRANT_READ_URI_PERMISSION | Intent.FLAG_GRANT_WRITE_URI_PERMISSION);
            } catch (Exception ignored) {
            }
            String local;
            if (saveName != null && saveName.toLowerCase().endsWith(".urdf")) {
                File urdf = new File(freshDir(), sanitize(saveName));
                nativePicked(id, urdf.getAbsolutePath(), "tree:" + uri);
                return;
            }
            if (saveName == null) {
                local = copyIn(uri, null);
            } else {
                local = new File(freshDir(), withExtension(displayName(uri, saveName), saveName)).getAbsolutePath();
            }
            nativePicked(id, local, uri.toString());
        } catch (Exception e) {
            Log.e(TAG, "picked document failed", e);
            nativePicked(id, null, null);
        }
    }

    /** Called from Rust: copy a local file the app wrote to its document. */
    public boolean writeBack(String local, String uri) {
        if (uri.startsWith("tree:")) return writeTree(new File(local).getParentFile(), Uri.parse(uri.substring(5)));
        try (InputStream in = new FileInputStream(local); OutputStream out = getContentResolver().openOutputStream(Uri.parse(uri), "wt")) {
            if (out == null) return false;
            copy(in, out);
            return true;
        } catch (Exception e) {
            Log.e(TAG, "write back to " + uri + " failed", e);
            return false;
        }
    }

    /** Copy a local export folder (a URDF and its meshes) into a picked folder, replacing files of the same name. */
    private boolean writeTree(File dir, Uri tree) {
        try {
            String rootId = DocumentsContract.getTreeDocumentId(tree);
            copyDir(dir, tree, rootId);
            return true;
        } catch (Exception e) {
            Log.e(TAG, "write to folder " + tree + " failed", e);
            return false;
        }
    }

    private void copyDir(File dir, Uri tree, String parentId) throws Exception {
        File[] files = dir.listFiles();
        if (files == null) return;
        ContentResolver cr = getContentResolver();
        Uri parent = DocumentsContract.buildDocumentUriUsingTree(tree, parentId);
        for (File f : files) {
            String[] found = child(tree, parentId, f.getName());
            if (f.isDirectory()) {
                String id;
                if (found != null && DocumentsContract.Document.MIME_TYPE_DIR.equals(found[1])) {
                    id = found[0];
                } else {
                    Uri made = DocumentsContract.createDocument(cr, parent, DocumentsContract.Document.MIME_TYPE_DIR, f.getName());
                    if (made == null) throw new Exception("could not create folder " + f.getName());
                    id = DocumentsContract.getDocumentId(made);
                }
                copyDir(f, tree, id);
                continue;
            }
            Uri target = found != null
                ? DocumentsContract.buildDocumentUriUsingTree(tree, found[0])
                : DocumentsContract.createDocument(cr, parent, mimeFor(f.getName()), f.getName());
            if (target == null) throw new Exception("could not create " + f.getName());
            try (InputStream in = new FileInputStream(f); OutputStream out = cr.openOutputStream(target, "wt")) {
                if (out == null) throw new Exception("no stream for " + target);
                copy(in, out);
            }
        }
    }

    /** A child of a tree folder by name: its document id and MIME type, or null. */
    private String[] child(Uri tree, String parentId, String name) {
        Uri kids = DocumentsContract.buildChildDocumentsUriUsingTree(tree, parentId);
        String[] cols = {DocumentsContract.Document.COLUMN_DOCUMENT_ID, DocumentsContract.Document.COLUMN_DISPLAY_NAME, DocumentsContract.Document.COLUMN_MIME_TYPE};
        try (Cursor c = getContentResolver().query(kids, cols, null, null, null)) {
            while (c != null && c.moveToNext()) {
                if (name.equals(c.getString(1))) return new String[] {c.getString(0), c.getString(2)};
            }
        } catch (Exception ignored) {
        }
        return null;
    }
    /** Called from Rust: the document's name as the provider shows it. */
    public String documentName(String uri) {
        return displayName(Uri.parse(uri), null);
    }

    public void setClipboard(String text) {
        runOnUiThread(() -> {
            ClipboardManager cm = (ClipboardManager) getSystemService(Context.CLIPBOARD_SERVICE);
            if (cm != null) cm.setPrimaryClip(ClipData.newPlainText("SolidCraft", text));
        });
    }

    public String getClipboard() {
        ClipboardManager cm = (ClipboardManager) getSystemService(Context.CLIPBOARD_SERVICE);
        if (cm == null || !cm.hasPrimaryClip() || cm.getPrimaryClip() == null || cm.getPrimaryClip().getItemCount() == 0) return null;
        CharSequence t = cm.getPrimaryClip().getItemAt(0).coerceToText(this);
        return t == null ? null : t.toString();
    }

    /** Called from Rust when a text field gains or loses focus; `password`: it is a password field. */
    public void showKeyboard(final boolean show, final boolean password) {
        runOnUiThread(() -> {
            InputMethodManager imm = (InputMethodManager) getSystemService(Context.INPUT_METHOD_SERVICE);
            View v = ime != null ? ime : getWindow().getDecorView();
            if (imm == null || v == null) return;
            if (ime != null) ime.password = show && password;
            if (show) {
                v.setFocusable(true);
                v.setFocusableInTouchMode(true);
                v.requestFocus();
                // A new field: the keyboard starts from an empty text, not the last field's.
                imm.restartInput(v);
                imm.showSoftInput(v, InputMethodManager.SHOW_FORCED);
            } else {
                imm.hideSoftInputFromWindow(v.getWindowToken(), 0);
            }
        });
    }

    /**
     * Called from Rust (a worker thread): one HTTP request of the agent chat's API agents, as the
     * desktop sends it: no redirects, at most 16 MB back. Returns the status (4 bytes, big-endian)
     * then the body; status 0 and the error's text when it failed.
     */
    public byte[] http(String method, String url, String[] headers, byte[] body, int timeoutS) {
        java.net.HttpURLConnection c = null;
        try {
            c = (java.net.HttpURLConnection) new java.net.URL(url).openConnection();
            c.setInstanceFollowRedirects(false);
            c.setConnectTimeout(10000);
            c.setReadTimeout(Math.max(1, timeoutS) * 1000);
            c.setRequestMethod(method);
            c.setRequestProperty("User-Agent", "SolidCraft");
            for (int i = 0; i + 1 < headers.length; i += 2) c.setRequestProperty(headers[i], headers[i + 1]);
            if (body != null) {
                c.setDoOutput(true);
                c.setFixedLengthStreamingMode(body.length);
                try (OutputStream out = c.getOutputStream()) {
                    out.write(body);
                }
            }
            int status = c.getResponseCode();
            InputStream in = status >= 400 ? c.getErrorStream() : c.getInputStream();
            java.io.ByteArrayOutputStream all = new java.io.ByteArrayOutputStream();
            all.write(new byte[] {(byte) (status >>> 24), (byte) (status >>> 16), (byte) (status >>> 8), (byte) status});
            if (in != null) {
                try (InputStream s = in) {
                    byte[] buf = new byte[1 << 16];
                    int n;
                    while ((n = s.read(buf)) > 0) {
                        if (all.size() + n > (16 << 20) + 4) throw new java.io.IOException("the response is larger than 16 MB");
                        all.write(buf, 0, n);
                    }
                }
            }
            return all.toByteArray();
        } catch (Exception e) {
            String m = e.getClass().getSimpleName() + (e.getMessage() == null ? "" : ": " + e.getMessage());
            byte[] t = m.getBytes(StandardCharsets.UTF_8);
            byte[] r = new byte[4 + t.length];
            System.arraycopy(t, 0, r, 4, t.length);
            return r;
        } finally {
            if (c != null) c.disconnect();
        }
    }
    // ---- Agent chat API keys: AES-GCM with a key that never leaves the Android Keystore; the
    // ciphertext (IV first) in private preferences, one entry per agent.

    private static final String KEY_ALIAS = "solidcraft-agent-chat";
    /** Network (SMB) location passwords: their own Keystore key and preferences, apart from the agents' keys. */
    private static final String SMB_KEY_ALIAS = "solidcraft-smb";

    private javax.crypto.SecretKey agentKey() throws Exception {
        return keystoreKey(KEY_ALIAS);
    }

    private javax.crypto.SecretKey keystoreKey(String alias) throws Exception {
        java.security.KeyStore ks = java.security.KeyStore.getInstance("AndroidKeyStore");
        ks.load(null);
        java.security.Key k = ks.getKey(alias, null);
        if (k instanceof javax.crypto.SecretKey) return (javax.crypto.SecretKey) k;
        javax.crypto.KeyGenerator g = javax.crypto.KeyGenerator.getInstance(KeyProperties.KEY_ALGORITHM_AES, "AndroidKeyStore");
        g.init(new KeyGenParameterSpec.Builder(alias, KeyProperties.PURPOSE_ENCRYPT | KeyProperties.PURPOSE_DECRYPT)
            .setBlockModes(KeyProperties.BLOCK_MODE_GCM)
            .setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_NONE)
            .setKeySize(256)
            .build());
        return g.generateKey();
    }

    private android.content.SharedPreferences secrets() {
        return getSharedPreferences("agent_chat_keys", MODE_PRIVATE);
    }

    /** Called from Rust: an agent's API key, or null when none is stored. */
    public String secretGet(String id) throws Exception {
        String v = secrets().getString(id, null);
        if (v == null) return null;
        byte[] b = Base64.decode(v, Base64.NO_WRAP);
        javax.crypto.Cipher c = javax.crypto.Cipher.getInstance("AES/GCM/NoPadding");
        c.init(javax.crypto.Cipher.DECRYPT_MODE, agentKey(), new javax.crypto.spec.GCMParameterSpec(128, b, 0, 12));
        return new String(c.doFinal(b, 12, b.length - 12), StandardCharsets.UTF_8);
    }

    public void secretSet(String id, String secret) throws Exception {
        javax.crypto.Cipher c = javax.crypto.Cipher.getInstance("AES/GCM/NoPadding");
        c.init(javax.crypto.Cipher.ENCRYPT_MODE, agentKey());
        byte[] iv = c.getIV();
        byte[] ct = c.doFinal(secret.getBytes(StandardCharsets.UTF_8));
        byte[] all = new byte[iv.length + ct.length];
        System.arraycopy(iv, 0, all, 0, iv.length);
        System.arraycopy(ct, 0, all, iv.length, ct.length);
        if (iv.length != 12) throw new Exception("unexpected GCM IV length " + iv.length);
        if (!secrets().edit().putString(id, Base64.encodeToString(all, Base64.NO_WRAP)).commit()) throw new Exception("could not save the key");
    }

    public void secretDelete(String id) throws Exception {
        if (!secrets().edit().remove(id).commit()) throw new Exception("could not delete the key");
    }

    private android.content.SharedPreferences smbSecrets() {
        return getSharedPreferences("smb_passwords", MODE_PRIVATE);
    }

    /** Called from Rust (worker threads too): a network location's password, or null. */
    public String smbSecretGet(String id) throws Exception {
        String v = smbSecrets().getString(id, null);
        if (v == null) return null;
        byte[] b = Base64.decode(v, Base64.NO_WRAP);
        javax.crypto.Cipher c = javax.crypto.Cipher.getInstance("AES/GCM/NoPadding");
        c.init(javax.crypto.Cipher.DECRYPT_MODE, keystoreKey(SMB_KEY_ALIAS), new javax.crypto.spec.GCMParameterSpec(128, b, 0, 12));
        return new String(c.doFinal(b, 12, b.length - 12), StandardCharsets.UTF_8);
    }

    public void smbSecretSet(String id, String secret) throws Exception {
        javax.crypto.Cipher c = javax.crypto.Cipher.getInstance("AES/GCM/NoPadding");
        c.init(javax.crypto.Cipher.ENCRYPT_MODE, keystoreKey(SMB_KEY_ALIAS));
        byte[] iv = c.getIV();
        if (iv.length != 12) throw new Exception("unexpected GCM IV length " + iv.length);
        byte[] ct = c.doFinal(secret.getBytes(StandardCharsets.UTF_8));
        byte[] all = new byte[iv.length + ct.length];
        System.arraycopy(iv, 0, all, 0, iv.length);
        System.arraycopy(ct, 0, all, iv.length, ct.length);
        if (!smbSecrets().edit().putString(id, Base64.encodeToString(all, Base64.NO_WRAP)).commit()) throw new Exception("could not save the password");
    }

    public void smbSecretDelete(String id) throws Exception {
        if (!smbSecrets().edit().remove(id).commit()) throw new Exception("could not delete the password");
    }

    /** Called from Rust: add or edit a network location with normal Android text fields (keyboard works). */
    public void smbConnectionForm(final int token, final String json) {
        runOnUiThread(() -> {
            try {
                JSONObject in = json != null && !json.isEmpty() ? new JSONObject(json) : new JSONObject();
                float d = getResources().getDisplayMetrics().density;
                int pad = (int) (12 * d);
                ScrollView scroll = new ScrollView(this);
                LinearLayout lay = new LinearLayout(this);
                lay.setOrientation(LinearLayout.VERTICAL);
                lay.setPadding(pad, pad, pad, pad);
                EditText name = addFormField(lay, "Name (optional)", in.optString("name", ""));
                EditText host = addFormField(lay, "Host", in.optString("host", ""));
                EditText share = addFormField(lay, "Share", in.optString("share", ""));
                EditText user = addFormField(lay, "User (empty = guest)", in.optString("user", ""));
                EditText domain = addFormField(lay, "Domain (optional)", in.optString("domain", ""));
                EditText password = addFormField(lay, "Password", "");
                password.setInputType(InputType.TYPE_CLASS_TEXT | InputType.TYPE_TEXT_VARIATION_PASSWORD);
                scroll.addView(lay);
                String title = in.optBoolean("editing", false) ? "Network location" : "Add network location";
                new AlertDialog.Builder(this)
                    .setTitle(title)
                    .setView(scroll)
                    .setPositiveButton("Save", (dialog, which) -> {
                        try {
                            JSONObject out = new JSONObject();
                            out.put("id", in.optString("id", ""));
                            out.put("name", name.getText().toString().trim());
                            out.put("host", host.getText().toString().trim());
                            out.put("share", share.getText().toString().trim());
                            out.put("user", user.getText().toString().trim());
                            out.put("domain", domain.getText().toString().trim());
                            out.put("password", password.getText().toString());
                            nativeSmbConn(token, out.toString());
                        } catch (Exception e) {
                            nativeSmbConn(token, null);
                        }
                    })
                    .setNegativeButton("Cancel", (dialog, which) -> nativeSmbConn(token, null))
                    .setOnCancelListener(dialog -> nativeSmbConn(token, null))
                    .show();
            } catch (Exception e) {
                Log.e(TAG, "smbConnectionForm", e);
                nativeSmbConn(token, null);
            }
        });
    }

    private static EditText addFormField(LinearLayout lay, String label, String value) {
        TextView t = new TextView(lay.getContext());
        t.setText(label);
        lay.addView(t);
        EditText e = new EditText(lay.getContext());
        e.setText(value);
        e.setSingleLine(true);
        LinearLayout.LayoutParams lp = new LinearLayout.LayoutParams(
            ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.WRAP_CONTENT);
        lp.bottomMargin = (int) (8 * lay.getContext().getResources().getDisplayMetrics().density);
        e.setLayoutParams(lp);
        lay.addView(e);
        return e;
    }

    /**
     * Called from Rust: Show in folder. A document from a picker opens its folder in Files (when
     * the provider names folders by path, as device storage does), else Files' picker starts in
     * it; a file only in the app's storage says so.
     */
    public void showInFolder(final String uri) {
        runOnUiThread(() -> {
            if (uri == null) {
                Toast.makeText(this, "This file is in SolidCraft's private storage, not in a folder Files can show.", Toast.LENGTH_LONG).show();
                return;
            }
            Uri u = Uri.parse(uri);
            try {
                if (DocumentsContract.isDocumentUri(this, u)) {
                    String id = DocumentsContract.getDocumentId(u);
                    int slash = id.lastIndexOf('/');
                    int colon = id.indexOf(':');
                    String parent = slash > colon ? id.substring(0, slash) : (colon >= 0 ? id.substring(0, colon + 1) : null);
                    if (parent != null) {
                        Uri dir = DocumentsContract.buildDocumentUri(u.getAuthority(), parent);
                        Intent v = new Intent(Intent.ACTION_VIEW);
                        v.setDataAndType(dir, DocumentsContract.Document.MIME_TYPE_DIR);
                        v.addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION);
                        startActivity(v);
                        return;
                    }
                }
            } catch (Exception e) {
                Log.i(TAG, "no folder view for " + uri + ": " + e);
            }
            try {
                Intent i = new Intent(Intent.ACTION_OPEN_DOCUMENT);
                i.addCategory(Intent.CATEGORY_OPENABLE);
                i.setType("*/*");
                i.putExtra(DocumentsContract.EXTRA_INITIAL_URI, u);
                startActivity(i);
            } catch (Exception e) {
                Log.e(TAG, "no Files app", e);
            }
        });
    }

    private File freshDir() {        File root = new File(getFilesDir(), "documents");
        for (int n = 0; ; n++) {
            File d = new File(root, Long.toString(System.currentTimeMillis(), 36) + "-" + n);
            if (!d.exists() && d.mkdirs()) return d;
        }
    }

    private String copyIn(Uri uri, String fallback) throws Exception {
        String name = sanitize(displayName(uri, fallback == null ? "document" : fallback));
        File f = new File(freshDir(), name);
        ContentResolver cr = getContentResolver();
        try (InputStream in = cr.openInputStream(uri); OutputStream out = new FileOutputStream(f)) {
            if (in == null) throw new Exception("no stream for " + uri);
            copy(in, out);
        }
        return f.getAbsolutePath();
    }

    private String displayName(Uri uri, String fallback) {
        try (Cursor c = getContentResolver().query(uri, new String[] {OpenableColumns.DISPLAY_NAME}, null, null, null)) {
            if (c != null && c.moveToFirst()) {
                String n = c.getString(0);
                if (n != null && !n.isEmpty()) return sanitize(n);
            }
        } catch (Exception ignored) {
        }
        String last = uri.getLastPathSegment();
        if (last != null && !last.isEmpty()) return sanitize(last.substring(last.lastIndexOf('/') + 1));
        return fallback;
    }

    private static String sanitize(String n) {
        return n.replace('/', '_').replace('\\', '_').replace(':', '_');
    }

    /** Keep the extension the app asked for: it picks the file format from it. */
    private static String withExtension(String name, String wanted) {
        int dot = wanted.lastIndexOf('.');
        if (dot < 0) return name;
        String ext = wanted.substring(dot);
        return name.toLowerCase().endsWith(ext.toLowerCase()) ? name : name + ext;
    }

    private static String mimeFor(String name) {
        int dot = name.lastIndexOf('.');
        String ext = dot < 0 ? "" : name.substring(dot + 1).toLowerCase();
        switch (ext) {
            case "json": return "application/json";
            case "png": return "image/png";
            case "gif": return "image/gif";
            case "pdf": return "application/pdf";
            case "svg": return "image/svg+xml";
            case "csv": return "text/csv";
            case "txt": return "text/plain";
            default:
                String m = MimeTypeMap.getSingleton().getMimeTypeFromExtension(ext);
                return m != null ? m : "application/octet-stream";
        }
    }

    private static void copy(InputStream in, OutputStream out) throws java.io.IOException {
        byte[] buf = new byte[1 << 16];
        int n;
        while ((n = in.read(buf)) > 0) out.write(buf, 0, n);
    }
}
