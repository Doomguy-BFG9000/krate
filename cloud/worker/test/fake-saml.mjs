/* A fake SAML 2.0 identity provider for the hub's tests (IC-955): what
 * Okta or Entra do with an SP-initiated sign-in, on this machine, with a
 * test key that nothing outside the tests trusts. Assertions are signed
 * with xml-crypto -- the same maintained library the hub verifies with,
 * running here in Node -- never by hand.
 *
 * - requestOf(location): the AuthnRequest a hub redirect carries (HTTP-
 *   Redirect binding: deflated, base64, in the query), read back.
 * - response({...}): a signed <samlp:Response> for one person, base64, as
 *   the HTTP-POST binding sends it. Every field a test wants to get wrong
 *   can be set: audience, recipient, times, issuer, signing key, ID.
 * - startIdp({...}): a real HTTP server that answers the redirect with the
 *   auto-posting form a browser follows, for a click-through against
 *   `wrangler dev`.
 */
import { readFileSync } from "node:fs";
import { inflateRawSync } from "node:zlib";
import { createServer } from "node:http";
import { randomBytes } from "node:crypto";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";
import { SignedXml } from "xml-crypto";

const here = join(dirname(fileURLToPath(import.meta.url)), "fixtures", "saml");
export const IDP = {
  key: readFileSync(join(here, "idp-key.pem"), "utf8"),
  cert: readFileSync(join(here, "idp-cert.pem"), "utf8"),
};
export const FORGER = {
  key: readFileSync(join(here, "forger-key.pem"), "utf8"),
  cert: readFileSync(join(here, "forger-cert.pem"), "utf8"),
};
export const IDP_ENTITY = "http://www.okta.com/exk-krate-test";

/* The AuthnRequest in a redirect to the IdP: its ID, where it wants the
 * answer, and who asked. */
export function requestOf(location) {
  const u = new URL(location);
  const xml = inflateRawSync(Buffer.from(u.searchParams.get("SAMLRequest"), "base64")).toString("utf8");
  const attr = (name) => (new RegExp(`\\b${name}="([^"]*)"`).exec(xml) || [])[1] || null;
  const issuer = (/<(?:saml2?:)?Issuer[^>]*>([^<]*)</.exec(xml) || [])[1] || null;
  return { xml, id: attr("ID"), acs: attr("AssertionConsumerServiceURL"), issuer, relayState: u.searchParams.get("RelayState"), idp: `${u.origin}${u.pathname}` };
}

const iso = (ms) => new Date(ms).toISOString().replace(/\.\d{3}Z$/, "Z");
const esc = (s) => String(s).replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;").replace(/"/g, "&quot;");

/* A signed SAML Response, base64. */
export function response({
  email,
  audience,
  recipient,
  inResponseTo,
  issuer = IDP_ENTITY,
  signer = IDP,
  assertionId = `_a${randomBytes(16).toString("hex")}`,
  now = Date.now(),
  lifetimeMs = 5 * 60 * 1000,
  notOnOrAfter,
  nameIdFormat = "urn:oasis:names:tc:SAML:1.1:nameid-format:emailAddress",
  sign = true,
  signResponse = false,
  tamper = null,
} = {}) {
  const until = iso(notOnOrAfter ?? now + lifetimeMs);
  const id = `_r${randomBytes(16).toString("hex")}`;
  const assertion =
    `<saml:Assertion xmlns:saml="urn:oasis:names:tc:SAML:2.0:assertion" ID="${esc(assertionId)}" IssueInstant="${iso(now)}" Version="2.0">` +
    `<saml:Issuer>${esc(issuer)}</saml:Issuer>` +
    `<saml:Subject><saml:NameID Format="${esc(nameIdFormat)}">${esc(email)}</saml:NameID>` +
    `<saml:SubjectConfirmation Method="urn:oasis:names:tc:SAML:2.0:cm:bearer">` +
    `<saml:SubjectConfirmationData InResponseTo="${esc(inResponseTo)}" NotOnOrAfter="${until}" Recipient="${esc(recipient)}"/>` +
    `</saml:SubjectConfirmation></saml:Subject>` +
    `<saml:Conditions NotBefore="${iso(now - 60_000)}" NotOnOrAfter="${until}">` +
    `<saml:AudienceRestriction><saml:Audience>${esc(audience)}</saml:Audience></saml:AudienceRestriction></saml:Conditions>` +
    `<saml:AuthnStatement AuthnInstant="${iso(now)}" SessionIndex="${id}"><saml:AuthnContext>` +
    `<saml:AuthnContextClassRef>urn:oasis:names:tc:SAML:2.0:ac:classes:PasswordProtectedTransport</saml:AuthnContextClassRef>` +
    `</saml:AuthnContext></saml:AuthnStatement>` +
    `<saml:AttributeStatement><saml:Attribute Name="email"><saml:AttributeValue>${esc(email)}</saml:AttributeValue></saml:Attribute></saml:AttributeStatement>` +
    `</saml:Assertion>`;
  let signed = assertion;
  if (sign) {
    const sig = new SignedXml({
      privateKey: signer.key,
      publicCert: signer.cert,
      signatureAlgorithm: "http://www.w3.org/2001/04/xmldsig-more#rsa-sha256",
      canonicalizationAlgorithm: "http://www.w3.org/2001/10/xml-exc-c14n#",
    });
    sig.addReference({
      xpath: "//*[local-name(.)='Assertion']",
      digestAlgorithm: "http://www.w3.org/2001/04/xmlenc#sha256",
      transforms: ["http://www.w3.org/2000/09/xmldsig#enveloped-signature", "http://www.w3.org/2001/10/xml-exc-c14n#"],
    });
    sig.computeSignature(assertion, { location: { reference: "//*[local-name(.)='Issuer']", action: "after" } });
    signed = sig.getSignedXml();
  }
  if (tamper) signed = tamper(signed);
  const xml =
    `<samlp:Response xmlns:samlp="urn:oasis:names:tc:SAML:2.0:protocol" xmlns:saml="urn:oasis:names:tc:SAML:2.0:assertion" ID="${id}" Version="2.0" IssueInstant="${iso(now)}" Destination="${esc(recipient)}" InResponseTo="${esc(inResponseTo)}">` +
    `<saml:Issuer>${esc(issuer)}</saml:Issuer>` +
    `<samlp:Status><samlp:StatusCode Value="urn:oasis:names:tc:SAML:2.0:status:Success"/></samlp:Status>` +
    signed +
    `</samlp:Response>`;
  if (signResponse) {
    // The envelope signed as well (or instead): what some IdPs do.
    const sig = new SignedXml({
      privateKey: signer.key,
      publicCert: signer.cert,
      signatureAlgorithm: "http://www.w3.org/2001/04/xmldsig-more#rsa-sha256",
      canonicalizationAlgorithm: "http://www.w3.org/2001/10/xml-exc-c14n#",
    });
    sig.addReference({
      xpath: "/*[local-name(.)='Response']",
      digestAlgorithm: "http://www.w3.org/2001/04/xmlenc#sha256",
      transforms: ["http://www.w3.org/2000/09/xmldsig#enveloped-signature", "http://www.w3.org/2001/10/xml-exc-c14n#"],
    });
    sig.computeSignature(xml, { location: { reference: "/*[local-name(.)='Response']/*[local-name(.)='Issuer']", action: "after" } });
    return Buffer.from(sig.getSignedXml(), "utf8").toString("base64");
  }
  return Buffer.from(xml, "utf8").toString("base64");
}

/* A fake IdP over HTTP: GET /sso answers with the form a browser posts to
 * the hub's ACS, signed for `email` (or the ?as= the test chose). */
export async function startIdp({ email = "erin@corp.example" } = {}) {
  const server = createServer((req, res) => {
    const u = new URL(req.url, "http://127.0.0.1");
    if (u.pathname !== "/sso") return res.writeHead(404).end();
    const r = requestOf(u.toString());
    const who = u.searchParams.get("as") || email;
    const body = response({ email: who, audience: r.issuer, recipient: r.acs, inResponseTo: r.id });
    res.writeHead(200, { "content-type": "text/html; charset=utf-8" });
    res.end(
      `<!doctype html><title>Fake IdP</title><body><p>Fake IdP: signing in ${esc(who)}</p>` +
        `<form method="post" action="${esc(r.acs)}"><input type="hidden" name="SAMLResponse" value="${body}">` +
        `<input type="hidden" name="RelayState" value="${esc(r.relayState || "")}"><button>Continue</button></form>` +
        `<script>document.forms[0].submit()</script>`,
    );
  });
  await new Promise((ok) => server.listen(0, "127.0.0.1", ok));
  return { url: `http://127.0.0.1:${server.address().port}/sso`, close: () => new Promise((ok) => server.close(ok)) };
}
