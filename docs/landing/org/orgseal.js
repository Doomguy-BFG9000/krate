/* Sealing to an organisation's key, in the browser (F-316).
 *
 * A company connection's credential is sealed here, on the admin's own
 * computer, to the organisation's public key, so what reaches Krate's hub is
 * ciphertext only a member's device can open. The suite and the bytes are
 * crates/orgkeys' `seal_for_org`, exactly: HPKE (RFC 9180) base mode,
 * DHKEM(X25519, HKDF-SHA256), HKDF-SHA256, ChaCha20-Poly1305; info is
 * "krate org seal v1", a zero byte, then the context; the associated data is
 * the organisation's public key; the result is base64 of the encapsulated
 * key followed by the ciphertext. The HPKE itself is the vendored hpke-js
 * (vendor/hpke-js-1.9.0.js): nothing here implements a primitive.
 *
 * crates/orgkeys/tests/vectors/org-seal.json is the proof the two agree: the
 * page test seals and opens it here, crates/orgkeys' tests there.
 */
(function (root) {
  "use strict";

  var INFO = "krate org seal v1\u0000";
  var SUITE = "RFC 9180 base mode, DHKEM(X25519, HKDF-SHA256), HKDF-SHA256, ChaCha20-Poly1305";

  function utf8(text) { return new TextEncoder().encode(String(text)); }

  function bytesOfHex(hex) {
    if (!/^([0-9a-fA-F]{2})+$/.test(String(hex || ""))) throw new Error("not hex");
    var out = new Uint8Array(hex.length / 2);
    for (var i = 0; i < out.length; i += 1) out[i] = parseInt(hex.substr(i * 2, 2), 16);
    return out;
  }

  function hexOf(bytes) {
    var b = new Uint8Array(bytes);
    var s = "";
    for (var i = 0; i < b.length; i += 1) s += (b[i] < 16 ? "0" : "") + b[i].toString(16);
    return s;
  }

  function b64(bytes) {
    var b = new Uint8Array(bytes);
    var s = "";
    for (var i = 0; i < b.length; i += 1) s += String.fromCharCode(b[i]);
    return btoa(s);
  }

  function unb64(text) {
    var s = atob(String(text || ""));
    var out = new Uint8Array(s.length);
    for (var i = 0; i < s.length; i += 1) out[i] = s.charCodeAt(i);
    return out;
  }

  function join(a, b) {
    var out = new Uint8Array(a.length + b.length);
    out.set(a);
    out.set(b, a.length);
    return out;
  }

  /// A copy as its own ArrayBuffer, which is what hpke-js takes.
  function buf(bytes) { return new Uint8Array(bytes).slice().buffer; }

  function suite() {
    var L = root.KrateHpkeLib;
    if (!L) throw new Error("the page's sealing library did not load: reload the page");
    return new L.CipherSuite({ kem: new L.DhkemX25519HkdfSha256(), kdf: new L.HkdfSha256(), aead: new L.Chacha20Poly1305() });
  }

  /// The context a connection's credential is sealed for: the organisation,
  /// the connection's name, its header and its address as the hub keeps it
  /// (crates/orgkeys `connection_context`, the same text).
  function connectionContext(org, name, header, base) {
    return "connection:" + org + "/" + name + "\n" + header + "\n" + base;
  }

  /// Seal `plaintext` to the organisation's public key (64 hex) for
  /// `context`: base64, as the hub keeps it. `testIkm` (64 hex) fixes the
  /// ephemeral key, for the cross-language vector only.
  function sealForOrg(publicHex, context, plaintext, testIkm) {
    var s = suite();
    var pub = bytesOfHex(publicHex);
    if (pub.length !== 32) return Promise.reject(new Error("the organisation's key is not an X25519 public key"));
    return s.kem.deserializePublicKey(buf(pub)).then(function (pk) {
      var params = { recipientPublicKey: pk, info: buf(join(utf8(INFO), utf8(context))) };
      if (testIkm) params.ekm = buf(bytesOfHex(testIkm));
      return s.createSenderContext(params);
    }).then(function (sender) {
      return sender.seal(buf(utf8(plaintext)), buf(pub)).then(function (ct) {
        return b64(join(new Uint8Array(sender.enc), new Uint8Array(ct)));
      });
    });
  }

  /// The organisation key pair RFC 9180 DeriveKeyPair makes from `ikmHex`:
  /// `{pair, publicHex}`. For tests: a browser never holds the private half.
  function deriveOrgKey(ikmHex) {
    var s = suite();
    return s.kem.deriveKeyPair(buf(bytesOfHex(ikmHex))).then(function (pair) {
      return s.kem.serializePublicKey(pair.publicKey).then(function (pub) {
        return { pair: pair, publicHex: hexOf(pub) };
      });
    });
  }

  /// Open what `sealForOrg` (or crates/orgkeys `seal_for_org`) sealed, with
  /// a pair from `deriveOrgKey`: the plaintext as text. For tests.
  function openForOrg(key, context, sealed) {
    var s = suite();
    var bytes = unb64(sealed);
    if (bytes.length < 48) return Promise.reject(new Error("not a sealed secret"));
    return s.createRecipientContext({ recipientKey: key.pair, enc: buf(bytes.slice(0, 32)), info: buf(join(utf8(INFO), utf8(context))) })
      .then(function (r) { return r.open(buf(bytes.slice(32)), buf(bytesOfHex(key.publicHex))); })
      .then(function (pt) { return new TextDecoder().decode(new Uint8Array(pt)); });
  }

  root.KrateOrgSeal = { SUITE: SUITE, connectionContext: connectionContext, sealForOrg: sealForOrg, deriveOrgKey: deriveOrgKey, openForOrg: openForOrg };
})(typeof window !== "undefined" ? window : globalThis);
