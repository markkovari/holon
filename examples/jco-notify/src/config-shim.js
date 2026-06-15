// Host shim for `wasi:config/runtime@0.2.0-draft`. Supplies the email/sms
// gateway base URLs the notify component POSTs to. Set via env so a test can
// point them at a local stub server (the contract names no vendor).

const values = {
  "notify:email-url": process.env.NOTIFY_EMAIL_URL ?? "",
  "notify:sms-url": process.env.NOTIFY_SMS_URL ?? "",
};

export function get(key) {
  const v = values[key];
  return v === undefined || v === "" ? undefined : v;
}
export function getAll() {
  return Object.entries(values);
}
