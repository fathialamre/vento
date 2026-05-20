// UUIDv7 generator (RFC 9562 draft).
//
// Layout (128 bits, big-endian):
//   - 48 bits unix_ts_ms  (timestamp in milliseconds)
//   - 4  bits version     (0x7)
//   - 12 bits rand_a      (random)
//   - 2  bits variant     (0b10)
//   - 62 bits rand_b      (random)
//
// Strict monotonicity: when called twice within the same millisecond — or if
// the system clock moves backwards — we increment the previous UUID's bytes
// by 1 (treating them as a 128-bit integer) instead of regenerating from
// scratch, so produced IDs are always strictly greater than the previous one.

const lastBytes = new Uint8Array(16);
let lastTimestamp = 0;
let hasLast = false;

export function newUuid(): string {
  const now = Date.now();
  const bytes = new Uint8Array(16);

  if (!hasLast || now > lastTimestamp) {
    lastTimestamp = now;
    writeTimestamp(bytes, now);
    crypto.getRandomValues(bytes.subarray(6));
  } else {
    // Same ms or backwards clock: increment last UUID by 1.
    incrementBigEndian(lastBytes);
    bytes.set(lastBytes);
  }

  // Version 7: top 4 bits of byte 6.
  bytes[6] = (bytes[6] & 0x0f) | 0x70;
  // RFC 4122 variant: top 2 bits of byte 8 are 10.
  bytes[8] = (bytes[8] & 0x3f) | 0x80;

  lastBytes.set(bytes);
  hasLast = true;
  return formatUuid(bytes);
}

export function uuidTimestamp(uuid: string): number {
  const hex = uuid.replace(/-/g, "").slice(0, 12);
  return parseInt(hex, 16);
}

function writeTimestamp(bytes: Uint8Array, ms: number): void {
  // 48-bit ms split into a 16-bit high half and a 32-bit low half so we can
  // use standard bitwise ops on each half (JS bitwise is 32-bit only).
  const high = Math.floor(ms / 0x1_0000_0000);
  const low = ms >>> 0;
  bytes[0] = (high >>> 8) & 0xff;
  bytes[1] = high & 0xff;
  bytes[2] = (low >>> 24) & 0xff;
  bytes[3] = (low >>> 16) & 0xff;
  bytes[4] = (low >>> 8) & 0xff;
  bytes[5] = low & 0xff;
}

function incrementBigEndian(bytes: Uint8Array): void {
  for (let i = bytes.length - 1; i >= 0; i--) {
    bytes[i] = (bytes[i] + 1) & 0xff;
    if (bytes[i] !== 0) return;
  }
}

function formatUuid(bytes: Uint8Array): string {
  let hex = "";
  for (let i = 0; i < 16; i++) hex += bytes[i].toString(16).padStart(2, "0");
  return (
    hex.slice(0, 8) +
    "-" +
    hex.slice(8, 12) +
    "-" +
    hex.slice(12, 16) +
    "-" +
    hex.slice(16, 20) +
    "-" +
    hex.slice(20, 32)
  );
}
