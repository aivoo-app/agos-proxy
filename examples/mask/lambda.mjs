/**
 * Egress mask — AWS Lambda function URL (Node 20+, response streaming).
 *
 * NOTE the Lambda limits before choosing this backend: 15 min invocation cap,
 * 6 MB request body (oversized bodies need the mask's --max-body-bytes guard),
 * and a static egress IP requires VPC + NAT, which then disables function-URL
 * streaming. For LLM relaying prefer a Cloudflare Worker or a plain VPS.
 *
 * Deploy (function URL, InvokeMode RESPONSE_STREAM):
 *   zip and `aws lambda create-function --handler index.handler ...`,
 *   then `aws lambda update-function-configuration --environment '{
 *     "Variables": {"MASK_SECRET": "<openssl rand -hex 24>"} }'`
 *   and create a function URL with auth NONE (the secret header is the gate).
 */
import { Buffer } from "node:buffer";
import https from "node:https";

export const handler = awslambda.streamifyResponse(async (event, responseStream, context) => {
  const headers = event.headers || {};
  const lower = Object.fromEntries(Object.entries(headers).map(([k, v]) => [k.toLowerCase(), v]));

  if (lower["x-forward-mask"] !== process.env.MASK_SECRET) {
    await reply(responseStream, 403, "forbidden\n", {});
    return;
  }

  // Probe: report our own egress identity via an IP echo service.
  if (lower["x-forward-probe"] === "ip") {
    const identity = await fetchJson("https://ifconfig.co/json");
    await reply(responseStream, 200, JSON.stringify(identity) + "\n", {});
    return;
  }

  const target = lower["x-forward-target"];
  if (!target || !/^https:\/\//.test(target)) {
    await reply(responseStream, 502, JSON.stringify({ error: "bad target" }) + "\n", {
      "x-forward-mask": "err",
    });
    return;
  }

  // Forward with passthrough streaming.
  const passthrough = new PassThroughWithMask("ok");
  try {
    const upstream = https.request(
      target,
      {
        method: event.requestContext?.http?.method || event.httpMethod || "POST",
        headers: forwardHeaders(lower),
      },
      (res) => {
        const meta = {
          statusCode: res.statusCode,
          headers: { ...res.headers, "x-forward-mask": "ok" },
        };
        awslambda.setResponseStreamMetadata(responseStream, meta);
        res.pipe(passthrough).pipe(responseStream);
      },
    );
    upstream.on("error", (e) => {
      passthrough.end(Buffer.from(JSON.stringify({ error: String(e) })));
    });
    if (event.body) {
      upstream.write(Buffer.from(event.body, event.isBase64Encoded ? "base64" : "utf8"));
    }
    upstream.end();
  } catch (e) {
    await reply(responseStream, 502, JSON.stringify({ error: String(e) }) + "\n", {
      "x-forward-mask": "err",
    });
  }
});

// --- helpers -------------------------------------------------------------

import { PassThrough } from "node:stream";

function PassThroughWithMask() {
  return new PassThrough();
}

function forwardHeaders(lower) {
  const out = {};
  for (const [k, v] of Object.entries(lower)) {
    if (/^(x-forward-|x-forwarded-)|^(host|content-length|accept-encoding)$/.test(k)) continue;
    out[k] = v;
  }
  return out;
}

function reply(stream, status, body, headers) {
  const meta = { statusCode: status, headers };
  awslambda.setResponseStreamMetadata(stream, meta);
  return new Promise((resolve) => {
    stream.end(body, resolve);
  });
}

function fetchJson(url) {
  return new Promise((resolve, reject) => {
    https.get(url, (res) => {
      let data = "";
      res.on("data", (c) => (data += c));
      res.on("end", () => {
        try {
          resolve(JSON.parse(data));
        } catch {
          resolve({ ip: data.trim(), asn: null, country: null });
        }
      });
    }).on("error", reject);
  });
}
