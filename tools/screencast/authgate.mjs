// Screencast: TOTP 2FA challenge-response. Enroll an account (secret sealed in
// the vault), activate with a code derived from the returned secret (as an
// authenticator app would), reveal single-use recovery codes, then log in with
// a live code and mint a session. The challenge-response axis in a real browser.
import { chromium } from "playwright";
import crypto from "node:crypto";

const BASE = process.env.AUTHGATE_URL || "http://127.0.0.1:3023";
const OUT = new URL("./videos/authgate/", import.meta.url).pathname;
const W = 560, H = 860;
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

// RFC 6238 TOTP (SHA1, 30s, 6 digits) from a base32 secret — what the app shows.
function base32Decode(s) {
  const alpha = "ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";
  let bits = "";
  for (const c of s.replace(/=+$/, "").toUpperCase()) bits += alpha.indexOf(c).toString(2).padStart(5, "0");
  const bytes = [];
  for (let i = 0; i + 8 <= bits.length; i += 8) bytes.push(parseInt(bits.slice(i, i + 8), 2));
  return Buffer.from(bytes);
}
function totp(secretB32) {
  const key = base32Decode(secretB32);
  const counter = Math.floor(Date.now() / 1000 / 30);
  const buf = Buffer.alloc(8);
  buf.writeBigUInt64BE(BigInt(counter));
  const h = crypto.createHmac("sha1", key).update(buf).digest();
  const o = h[h.length - 1] & 0xf;
  const code = ((h.readUInt32BE(o) & 0x7fffffff) % 1_000_000).toString().padStart(6, "0");
  return code;
}

const browser = await chromium.launch({ headless: true });
const ctx = await browser.newContext({
  viewport: { width: W, height: H },
  recordVideo: { dir: OUT, size: { width: W, height: H } },
});
const page = await ctx.newPage();
await page.goto(BASE);

try {
  await sleep(900);

  // ① enroll -> secret sealed in the vault, uri shown.
  await page.click("#enroll");
  await sleep(1800);
  const secret = (await page.locator("#secret").textContent()).trim();

  // ② activate with the first code derived from the secret.
  await page.fill("#activateCode", totp(secret));
  await sleep(500);
  await page.click("#activate");
  await sleep(2000); // recovery codes reveal

  // ③ log in with a fresh live code -> a session.
  await page.fill("#loginCode", totp(secret));
  await sleep(500);
  await page.click("#login");
  await sleep(2200);
} finally {
  await ctx.close();
  await browser.close();
}
console.log("done");
