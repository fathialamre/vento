import { describe, expect, it } from "vitest";
import { newUuid, uuidTimestamp } from "@/lib/uuid";

describe("newUuid", () => {
  it("returns RFC 4122 form: 8-4-4-4-12 lowercase hex", () => {
    const id = newUuid();
    expect(id).toMatch(
      /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/,
    );
  });

  it("encodes version 7 and the RFC 4122 variant bits", () => {
    const id = newUuid();
    // Version nibble lives at position 14 (right after the second dash).
    expect(id[14]).toBe("7");
    // Variant nibble at position 19 must be 8, 9, a, or b (top two bits = 10).
    expect("89ab").toContain(id[19]);
  });

  it("produces 10k unique ids", () => {
    const ids = new Set<string>();
    for (let i = 0; i < 10_000; i++) ids.add(newUuid());
    expect(ids.size).toBe(10_000);
  });

  it("is strictly monotonic under lexicographic order", () => {
    // UUIDv7's ms-prefix + per-ms counter increment makes string comparison
    // equivalent to generation order. The sync engine relies on this for
    // 'newer wins' merges without an extra updated_at lookup.
    let prev = newUuid();
    for (let i = 0; i < 5_000; i++) {
      const next = newUuid();
      expect(next > prev).toBe(true);
      prev = next;
    }
  });

  it("recovers the generation timestamp via uuidTimestamp()", () => {
    const before = Date.now();
    const id = newUuid();
    const after = Date.now();
    const ts = uuidTimestamp(id);
    expect(ts).toBeGreaterThanOrEqual(before);
    expect(ts).toBeLessThanOrEqual(after);
  });
});
