import { describe, expect, it } from "vitest";
import { formatDuration, shortenPath, truncatePath } from "./format";

describe("formatDuration", () => {
  it("1分より短ければ、秒を小数1桁で出す", () => {
    expect(formatDuration(0)).toBe("0.0s");
    expect(formatDuration(340)).toBe("0.3s");
    expect(formatDuration(8_440)).toBe("8.4s");
    expect(formatDuration(59_900)).toBe("59.9s");
  });

  it("1時間より短ければ、分と秒で出す", () => {
    expect(formatDuration(59_960)).toBe("1m 00s");
    expect(formatDuration(348_000)).toBe("5m 48s");
  });

  it("それより長ければ、時間と分で出す", () => {
    expect(formatDuration(3_600_000)).toBe("1h 00m");
    expect(formatDuration(6_600_000)).toBe("1h 50m");
  });

  it("負の値は 0 にする", () => {
    expect(formatDuration(-5)).toBe("0.0s");
  });
});

describe("truncatePath", () => {
  it("短ければ変えない", () => {
    expect(truncatePath("~/work/app", 20)).toBe("~/work/app");
  });

  it("長ければ先頭を省いて、収まるだけの末尾を残す", () => {
    expect(truncatePath("~/work/projects/shop-api/src/payment", 24)).toBe("…/shop-api/src/payment");
    expect(truncatePath("/a/b/c/d", 5)).toBe("…/c/d");
  });

  it("最後の名前が長くても、その名前は省かない", () => {
    expect(truncatePath("/tmp/a-very-long-directory-name", 10)).toBe(
      "…/a-very-long-directory-name",
    );
  });
});

describe("shortenPath", () => {
  it("ホームディレクトリを ~ にする", () => {
    expect(shortenPath("/home/me", "/home/me")).toBe("~");
    expect(shortenPath("/home/me/work/app", "/home/me")).toBe("~/work/app");
  });

  it("ホームの外や、名前が似ているだけのディレクトリは変えない", () => {
    expect(shortenPath("/tmp", "/home/me")).toBe("/tmp");
    expect(shortenPath("/home/merry", "/home/me")).toBe("/home/merry");
    expect(shortenPath("/tmp", null)).toBe("/tmp");
  });
});
