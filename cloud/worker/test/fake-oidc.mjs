/* A fake OpenID Connect issuer, for the company sign-in tests (IC-948, F).
 *
 * A real node HTTP server on this machine's loopback with its own keys:
 * discovery, authorize, token and JWKS, the way Google Workspace, Okta or
 * Entra answer them. It never talks to a real provider. It also answers
 * DNS over HTTPS (JSON form) at /dns-query from a table the test fills, so
 * the domain proof can be exercised without public DNS.
 *
 * What a test can steer:
 *   issuer.person = { sub, email, email_verified, name }   who "signs in"
 *   issuer.tamper = (claims, header) => ...                 change the token
 *   issuer.signWith = "other"                               a key not in JWKS
 *   issuer.txt.set(name, [values])                          DNS answers
 *   issuer.reusableCodes = true          a sloppy issuer that takes a code twice
 *   issuer.discoveryIssuer = "..."       discovery that names another issuer
 *
 * Its token endpoint checks what a strict issuer checks: the client id and
 * secret, the redirect URI, and the PKCE verifier against the challenge.
 *
 * Standalone, for the `wrangler dev` proof:
 *   node cloud/worker/test/fake-oidc.mjs 9555 <client_id> <client_secret> <email>
 * and steered there with POST /test/txt {name, value} and POST /test/person.
 */
import { createServer } from "node:http";
import { generateKeyPairSync, createSign, createHash, randomBytes } from "node:crypto";

const b64url = (buf) => Buffer.from(buf).toString("base64url");

function keyPair(kid) {
  const { privateKey, publicKey } = generateKeyPairSync("rsa", { modulusLength: 2048 });
  const jwk = publicKey.export({ format: "jwk" });
  return { kid, privateKey, jwk: { ...jwk, kid, use: "sig", alg: "RS256" } };
}

export async function startIssuer({ clientId = "krate-client", clientSecret = "s3cret-for-tests", port = 0 } = {}) {
  const main = keyPair("main-1");
  const other = keyPair("main-1"); // same kid, different key: a forged signature
  const codes = new Map();
  const issuer = {
    clientId,
    clientSecret,
    person: { sub: "u-1001", email: "erin@corp.example", email_verified: true, name: "Erin" },
    tamper: null,
    signWith: "main",
    txt: new Map(),
    tokenRequests: [],
    reusableCodes: false,
    discoveryIssuer: null,
    url: "",
  };

  function idToken(claims) {
    const header = { alg: "RS256", typ: "JWT", kid: main.kid };
    if (issuer.tamper) issuer.tamper(claims, header);
    const input = `${b64url(JSON.stringify(header))}.${b64url(JSON.stringify(claims))}`;
    const signer = createSign("RSA-SHA256");
    signer.update(input);
    const key = issuer.signWith === "other" ? other.privateKey : main.privateKey;
    return `${input}.${b64url(signer.sign(key))}`;
  }

  const send = (res, status, body, headers = {}) => {
    res.writeHead(status, { "content-type": "application/json", ...headers });
    res.end(typeof body === "string" ? body : JSON.stringify(body));
  };

  const server = createServer(async (req, res) => {
    const u = new URL(req.url, issuer.url);
    let raw = "";
    for await (const chunk of req) raw += chunk;
    if (u.pathname === "/.well-known/openid-configuration") {
      return send(res, 200, {
        issuer: issuer.discoveryIssuer || issuer.url,
        authorization_endpoint: `${issuer.url}/authorize`,
        token_endpoint: `${issuer.url}/token`,
        jwks_uri: `${issuer.url}/jwks`,
        response_types_supported: ["code"],
        id_token_signing_alg_values_supported: ["RS256"],
        token_endpoint_auth_methods_supported: ["client_secret_post", "client_secret_basic"],
        code_challenge_methods_supported: ["S256"],
      });
    }
    if (u.pathname === "/jwks") return send(res, 200, { keys: [main.jwk] });
    if (u.pathname === "/authorize") {
      const p = u.searchParams;
      if (p.get("client_id") !== issuer.clientId || p.get("response_type") !== "code" || p.get("code_challenge_method") !== "S256" || !p.get("code_challenge") || !p.get("nonce") || !p.get("state")) {
        return send(res, 400, { error: "invalid_request" });
      }
      const code = randomBytes(16).toString("hex");
      codes.set(code, {
        challenge: p.get("code_challenge"),
        nonce: p.get("nonce"),
        redirect: p.get("redirect_uri"),
        person: { ...issuer.person },
      });
      const back = new URL(p.get("redirect_uri"));
      back.searchParams.set("code", code);
      back.searchParams.set("state", p.get("state"));
      res.writeHead(302, { location: back.toString() });
      return res.end();
    }
    if (u.pathname === "/token" && req.method === "POST") {
      const form = new URLSearchParams(raw);
      issuer.tokenRequests.push(Object.fromEntries(form));
      let id = form.get("client_id");
      let secret = form.get("client_secret");
      const basic = /^Basic (.+)$/.exec(req.headers.authorization || "");
      if (basic) {
        const [a, b] = Buffer.from(basic[1], "base64").toString().split(":");
        id = decodeURIComponent(a);
        secret = decodeURIComponent(b);
      }
      const grant = codes.get(form.get("code"));
      if (!issuer.reusableCodes) codes.delete(form.get("code"));
      if (!grant || id !== issuer.clientId || secret !== issuer.clientSecret) return send(res, 401, { error: "invalid_client" });
      if (form.get("redirect_uri") !== grant.redirect) return send(res, 400, { error: "invalid_grant" });
      const verifier = form.get("code_verifier") || "";
      if (b64url(createHash("sha256").update(verifier).digest()) !== grant.challenge) return send(res, 400, { error: "invalid_grant" });
      const now = Math.floor(Date.now() / 1000);
      const claims = {
        iss: issuer.url,
        aud: issuer.clientId,
        sub: grant.person.sub,
        email: grant.person.email,
        email_verified: grant.person.email_verified,
        name: grant.person.name,
        nonce: grant.nonce,
        iat: now,
        exp: now + 300,
      };
      return send(res, 200, { access_token: "at", token_type: "Bearer", id_token: idToken(claims) });
    }
    if (u.pathname === "/dns-query") {
      const name = String(u.searchParams.get("name") || "").toLowerCase();
      const values = issuer.txt.get(name) || [];
      return send(res, 200, { Status: 0, Answer: values.map((v) => ({ name, type: 16, TTL: 60, data: JSON.stringify(v) })) }, { "content-type": "application/dns-json" });
    }
    // Steering for the standalone proof, where the test is another process.
    if (u.pathname === "/test/txt" && req.method === "POST") {
      const { name, value } = JSON.parse(raw);
      issuer.txt.set(String(name).toLowerCase(), [value]);
      return send(res, 200, { ok: true });
    }
    if (u.pathname === "/test/person" && req.method === "POST") {
      issuer.person = JSON.parse(raw);
      return send(res, 200, { ok: true });
    }
    send(res, 404, { error: "not_found" });
  });
  await new Promise((resolve) => server.listen(port, "127.0.0.1", resolve));
  issuer.url = `http://127.0.0.1:${server.address().port}`;
  issuer.close = () => new Promise((resolve) => server.close(resolve));
  return issuer;
}

if (import.meta.url === `file://${process.argv[1]}`) {
  const [port, clientId, clientSecret, email] = process.argv.slice(2);
  const issuer = await startIssuer({ port: Number(port), clientId, clientSecret });
  issuer.person = { sub: `sub-${email}`, email, email_verified: true, name: email.split("@")[0] };
  console.log(`fake issuer at ${issuer.url}`);
}
