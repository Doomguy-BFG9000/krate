/* GitHub, for tests that need an admin (K-1085).
 *
 * KRATE_ADMINS names GitHub logins, and only a login GitHub itself vouches
 * for may match it: an email or Google session (krs_...) never can. So a
 * test admin signs in with a GitHub token. This stands in for GitHub: a
 * token `gho_<login>` is answered as that GitHub user; any other token GitHub
 * refuses. Everything that is not api.github.com/user goes on to whatever
 * fetch was there before.
 */
const before = globalThis.fetch;
globalThis.fetch = async (input, init = {}) => {
  const url = String((input && input.url) || input);
  if (url === "https://api.github.com/user") {
    const headers = new Headers((input && input.headers) || init.headers || {});
    const token = (headers.get("authorization") || "").replace(/^Bearer\s+/i, "");
    const login = token.startsWith("gho_") ? token.slice(4) : "";
    if (!login) return new Response("Bad credentials", { status: 401 });
    return new Response(JSON.stringify({ id: login.length * 1000 + login.charCodeAt(0), login, name: login }), {
      headers: { "content-type": "application/json" },
    });
  }
  return before(input, init);
};
