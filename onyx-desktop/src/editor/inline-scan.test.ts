// Unit tests for the pure inline scanner — must mirror onyx-md semantics.

import { describe, expect, it } from "vitest";

import { scanInline } from "./inline-scan";

describe("wikilinks", () => {
  it("scans a plain wikilink", () => {
    const { links } = scanInline("See [[Other Note]] here");
    expect(links).toHaveLength(1);
    expect(links[0]).toMatchObject({
      start: 4,
      end: 18,
      target: "Other Note",
      alias: null,
      embed: false,
    });
  });

  it("handles alias, heading, and block refs", () => {
    const { links } = scanInline("[[Note#Section|shown]] [[N#^blk]]");
    expect(links[0]).toMatchObject({ target: "Note", alias: "shown" });
    expect(links[1]).toMatchObject({ target: "N" });
  });

  it("display range covers alias when present", () => {
    const text = "[[Target|Alias]]";
    const { links } = scanInline(text);
    const link = links[0]!;
    expect(text.slice(link.displayStart, link.displayEnd)).toBe("Alias");
  });

  it("display range covers inner when no alias", () => {
    const text = "x [[folder/Target]] y";
    const { links } = scanInline(text);
    const link = links[0]!;
    expect(text.slice(link.displayStart, link.displayEnd)).toBe("folder/Target");
  });

  it("detects embeds", () => {
    const { links } = scanInline("![[image.png]]");
    expect(links[0]).toMatchObject({ embed: true, target: "image.png", start: 0 });
  });

  it("same-file heading link has empty target", () => {
    const { links } = scanInline("[[#Heading]]");
    expect(links[0]).toMatchObject({ target: "" });
  });

  it("recovers from stray openers", () => {
    const { links } = scanInline("[[a [[b]]");
    expect(links).toHaveLength(1);
    expect(links[0]!.target).toBe("b");
  });

  it("rejects empty and unclosed links", () => {
    expect(scanInline("[[]]").links).toHaveLength(0);
    expect(scanInline("[[unclosed").links).toHaveLength(0);
  });

  it("respects escapes", () => {
    expect(scanInline("\\[[not a link]]").links).toHaveLength(0);
  });

  it("offsets apply to all ranges", () => {
    const { links } = scanInline("[[a]]", 100);
    expect(links[0]).toMatchObject({ start: 100, end: 105 });
  });
});

describe("tags", () => {
  it("scans basic and nested tags", () => {
    const { tags } = scanInline("#tag and #nested/tag here");
    expect(tags.map((tag) => tag.tag)).toEqual(["tag", "nested/tag"]);
  });

  it("requires whitespace or line start before #", () => {
    expect(scanInline("foo#bar").tags).toHaveLength(0);
    expect(scanInline("#start").tags).toHaveLength(1);
  });

  it("rejects all-numeric tags, accepts mixed", () => {
    const { tags } = scanInline("#123 #1a");
    expect(tags.map((tag) => tag.tag)).toEqual(["1a"]);
  });

  it("trims trailing slashes", () => {
    expect(scanInline("#a/b/").tags[0]!.tag).toBe("a/b");
  });

  it("supports unicode tags", () => {
    expect(scanInline("#日本語 #ünïcode").tags.map((tag) => tag.tag)).toEqual([
      "日本語",
      "ünïcode",
    ]);
  });

  it("escaped hash is not a tag", () => {
    expect(scanInline("\\#nope").tags).toHaveLength(0);
  });

  it("span includes the hash", () => {
    const text = "a #tag b";
    const tag = scanInline(text).tags[0]!;
    expect(text.slice(tag.start, tag.end)).toBe("#tag");
  });
});

describe("interaction between constructs", () => {
  it("hash inside a wikilink is not a tag", () => {
    const { links, tags } = scanInline("[[Note#Heading]]");
    expect(links).toHaveLength(1);
    expect(tags).toHaveLength(0);
  });

  it("multiple links keep correct order and spans", () => {
    const text = "[[a]] mid [[b|c]] end";
    const { links } = scanInline(text);
    expect(text.slice(links[0]!.start, links[0]!.end)).toBe("[[a]]");
    expect(text.slice(links[1]!.start, links[1]!.end)).toBe("[[b|c]]");
  });
});
