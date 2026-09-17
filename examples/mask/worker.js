/**
 * Egress mask — Cloudflare Worker.
 *
 * Forwards every request to the absolute upstream URL given in
 * `X-forward-target`, streaming the body untouched, and answers the probe
 * endpoint with this Worker's own egress identity.
 *
 * Deploy:
 *   1. `npx wrangler init mask-hop --workers` (no starter code)
 *   2. put this file in `src/index.js`, add to `wrangler.toml`:
 *        [vars]
 *        MASK_SECRET = "<generate: openssl rand -hex 24>"
 *   3. `npx wrangler deploy`
 *   4. register with AGOS:
 *      agos-proxy mask add --name hop1 --kind cf_worker \
 *        --endpoint-url https://<your-worker>.workers.dev --secret $MASK_SECRET
 */

const STRIP_REQUEST = /^x-forward-|^cf-|^x-forwarded-$/i;

export default {
  async fetch(request, env) {
    const secret = request.headers.get("x-forward-mask");
    if (!secret || secret !== env.MASK_SECRET) {
      return new Response("forbidden\n", { status: 403 });
    }

    // Probe: report our own egress identity (for `mask test/audit`).
    if (request.headers.get("x-forward-probe") === "ip") {
      return json({
        ip: request.cf.clientIp || "unknown",
        asn: request.cf.asn ? `AS${request.cf.asn} ${request.cf.asOrganization || ""}` : null,
        country: request.cf.country || null,
      });
    }

    const target = request.headers.get("x-forward-target");
    if (!target || !/^https:\/\//.test(target)) {
      return replyErr("missing or invalid X-forward-target");
    }

    // Forward: same method, streamed body, stripped hop headers.
    const headers = new Headers();
    request.headers.forEach((value, name) => {
      if (!STRIP_REQUEST.test(name) && name.toLowerCase() !== "host") {
        headers.set(name, value);
      }
    });

    try {
      const upstream = await fetch(target, {
        method: request.method,
        headers,
        body: request.body,
        redirect: "manual",
        // @ts-ignore CF-specific: stream without buffering
        duplex: "half",
      });

      const out = new Headers(upstream.headers);
      out.set("x-forward-mask", "ok");
      return new Response(upstream.body, { status: upstream.status, headers: out });
    } catch (e) {
      return replyErr(String(e));
    }
  },
};

function replyErr(message) {
  return json({ error: message }, 502, { "x-forward-mask": "err" });
}

function json(obj, status = 200, extra = {}) {
  return new Response(JSON.stringify(obj) + "\n", {
    status,
    headers: { "content-type": "application/json", ...extra },
  });
}
