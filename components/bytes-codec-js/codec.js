// `bytes:codec`, in JavaScript — the same WIT the Rust component exports.
//
// The third language tried against this contract. The point is not that JavaScript
// has a base64 (it has two); it is whether a capability written in a language nothing
// else in this pool uses is judged by the specification that already exists, without
// the gate or the probe being touched.
//
// Deliberately not `btoa`/`Buffer`: this must agree with the Rust implementation on
// both alphabets and on padding, and a standard library that quietly differs on
// either would prove less, not more.

const STD = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
const URL = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

/**
 * The value of one character in `alphabet`, or -1.
 *
 * The two alphabets are NOT interchangeable: a character belonging to the other table
 * is refused rather than accepted because some table has it. Decoding with the wrong
 * one otherwise returns different bytes and reports nothing.
 */
function value(c, alphabet) {
  if (c >= "A" && c <= "Z") return c.charCodeAt(0) - 65;
  if (c >= "a" && c <= "z") return c.charCodeAt(0) - 97 + 26;
  if (c >= "0" && c <= "9") return c.charCodeAt(0) - 48 + 52;
  if (alphabet === "standard") {
    if (c === "+") return 62;
    if (c === "/") return 63;
  } else {
    if (c === "-") return 62;
    if (c === "_") return 63;
  }
  return -1;
}

const notInAlphabet = (at, found) => ({ tag: "not-in-alphabet", val: [at, found] });
const truncated = (n) => ({ tag: "truncated-group", val: n });
const misplaced = (at) => ({ tag: "misplaced-padding", val: at });

export const codec = {
  encode(bytes, alphabet) {
    const table = alphabet === "standard" ? STD : URL;
    const padded = alphabet === "standard";
    let out = "";
    for (let i = 0; i < bytes.length; i += 3) {
      const remaining = bytes.length - i;
      const n = (bytes[i] << 16) | ((remaining > 1 ? bytes[i + 1] : 0) << 8) | (remaining > 2 ? bytes[i + 2] : 0);
      out += table[(n >> 18) & 0x3f] + table[(n >> 12) & 0x3f];
      if (remaining > 2) out += table[(n >> 6) & 0x3f] + table[n & 0x3f];
      else if (remaining === 2) out += table[(n >> 6) & 0x3f] + (padded ? "=" : "");
      else out += padded ? "==" : "";
    }
    return out;
  },

  decode(text, alphabet) {
    if (text.length === 0) return new Uint8Array(0);

    // Padding belongs at the end and nowhere else.
    let padStart = text.indexOf("=");
    if (padStart < 0) padStart = text.length;
    for (let i = padStart; i < text.length; i++) {
      if (text[i] !== "=") throw misplaced(padStart);
    }
    if (text.length - padStart > 2) throw misplaced(padStart);

    const values = [];
    for (let i = 0; i < padStart; i++) {
      const v = value(text[i], alphabet);
      if (v < 0) throw notInAlphabet(i, text[i]);
      values.push(v);
    }
    // One leftover character encodes no byte.
    if (values.length % 4 === 1) throw truncated(text.length);

    const out = [];
    let i = 0;
    for (; i + 4 <= values.length; i += 4) {
      const n = (values[i] << 18) | (values[i + 1] << 12) | (values[i + 2] << 6) | values[i + 3];
      out.push((n >> 16) & 0xff, (n >> 8) & 0xff, n & 0xff);
    }
    const rest = values.length - i;
    if (rest === 2) out.push(((values[i] << 18) | (values[i + 1] << 12)) >> 16 & 0xff);
    else if (rest === 3) {
      const n = (values[i] << 18) | (values[i + 1] << 12) | (values[i + 2] << 6);
      out.push((n >> 16) & 0xff, (n >> 8) & 0xff);
    }
    return new Uint8Array(out);
  },

  toHex(bytes) {
    let out = "";
    for (const b of bytes) out += b.toString(16).padStart(2, "0");
    return out;
  },

  fromHex(text) {
    const values = [];
    for (let i = 0; i < text.length; i++) {
      const c = text[i];
      let v;
      if (c >= "0" && c <= "9") v = c.charCodeAt(0) - 48;
      else if (c >= "a" && c <= "f") v = c.charCodeAt(0) - 97 + 10;
      else if (c >= "A" && c <= "F") v = c.charCodeAt(0) - 65 + 10;
      else throw notInAlphabet(i, c);
      values.push(v);
    }
    if (values.length % 2 !== 0) throw truncated(values.length);
    const out = [];
    for (let i = 0; i < values.length; i += 2) out.push((values[i] << 4) | values[i + 1]);
    return new Uint8Array(out);
  },
};
