// Screencast: passwordless passkey sign-in (React + shadcn UI) on the REAL
// running app, driven by Chromium's CDP VIRTUAL AUTHENTICATOR — a real CTAP2
// authenticator implementation with a real key pair, minus the OS biometric
// prompt (which is exactly what makes this recordable at all).
//
// The story, in order:
//   1. enrol: a username, one click, no password anywhere -> signed in
//   2. the passkey appears: ES256, user-verified, its signature counter
//   3. add a second device (a second virtual authenticator) -> two passkeys
//   4. sign out, then sign in with NO USERNAME at all — the authenticator
//      offers its discoverable passkey and the credential id identifies the
//      account
//
// Prereq: from repo root  `just host-passkey &`  (SPA on :3053).
// Note: the host's kv is in-memory, so RESTART it before re-recording — "ada"
// already existing makes step 1 (correctly) refuse to enrol without a session.
import { chromium } from "playwright";

// Must be localhost, not 127.0.0.1: the RP ID is `localhost` and WebAuthn ties
// the credential to it.
const BASE = process.env.PASSKEY_URL || "http://localhost:3053";
const OUT = new URL("./videos/passkey/", import.meta.url).pathname;
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

const browser = await chromium.launch({ headless: true });
const ctx = await browser.newContext({
  viewport: { width: 760, height: 560},
  recordVideo: { dir: OUT, size: { width: 760, height: 560} },
  deviceScaleFactor: 1,
});
const page = await ctx.newPage();
const cdp = await ctx.newCDPSession(page);
await cdp.send("WebAuthn.enable");

/// A virtual authenticator that stores discoverable credentials and reports the
/// user as verified — i.e. a platform passkey (Touch ID / Windows Hello).
const addAuthenticator = async (transport) => {
  const { authenticatorId } = await cdp.send("WebAuthn.addVirtualAuthenticator", {
    options: {
      protocol: "ctap2",
      transport,
      hasResidentKey: true,
      hasUserVerification: true,
      isUserVerified: true,
      automaticPresenceSimulation: true,
    },
  });
  return authenticatorId;
};

const laptop = await addAuthenticator("internal");
await page.goto(BASE);

try {
  await sleep(1000);

  // 1. enrol — a username and one click.
  await page.getByPlaceholder("username").fill("ada");
  await sleep(700);
  await page.getByRole("button", { name: "Create a passkey" }).click();
  await sleep(2600); // signed in; the passkey row appears

  // 2. add a second device. The first authenticator is in `excludeCredentials`
  // (one device may not enrol twice), so hand presence to a second one — a USB
  // key standing in for "my phone".
  await cdp.send("WebAuthn.setAutomaticPresenceSimulation", { authenticatorId: laptop, enabled: false });
  await addAuthenticator("usb");
  await sleep(600);
  await page.getByRole("button", { name: "Add another device" }).click();
  await sleep(2600); // two passkeys listed

  // 3. sign out...
  await page.getByRole("button", { name: "Sign out" }).click();
  await sleep(1600);

  // 4. ...and back in with no username: the authenticator offers the passkey it
  // holds for this site, and the credential id tells the server who that is.
  await page.getByRole("button", { name: "Sign in without a username" }).click();
  await sleep(3000);
} finally {
  await ctx.close();
  await browser.close();
}
console.log("done");
