// Capability policy tests — the security table the broker enforces.

import { describe, expect, it } from "vitest";

import { requiredCapability } from "./host";

describe("plugin capability policy", () => {
  it("maps read surfaces to vault:read", () => {
    expect(requiredCapability("vault.read")).toBe("vault:read");
    expect(requiredCapability("vault.list")).toBe("vault:read");
  });

  it("maps writes to vault:write", () => {
    expect(requiredCapability("vault.write")).toBe("vault:write");
  });

  it("maps command registration to ui:commands", () => {
    expect(requiredCapability("commands.register")).toBe("ui:commands");
  });

  it("notices are always allowed", () => {
    expect(requiredCapability("notice")).toBeNull();
  });

  it("unknown methods are always rejected (undefined, not null)", () => {
    expect(requiredCapability("fs.readAnything")).toBeUndefined();
    expect(requiredCapability("network.fetch")).toBeUndefined();
    expect(requiredCapability("")).toBeUndefined();
  });
});
