// The channel's envelope is a contract: `id` is the reply ticket and `cmd`
// names the command. A payload field called either silently overwrote the
// envelope once — `chat.decide` sent the approval's id as `id`, the answer was
// addressed to a ticket the page never had, and the card sat disabled until
// its timeout. These pin the refusal and the ordinary round trip.
import { describe, expect, it } from "vitest";
import { call } from "./api";
import { installChannel } from "./testChannel";

describe("the page's command channel", () => {
  it("refuses a payload field that would overwrite the envelope", async () => {
    installChannel(() => ({ ok: true }));

    await expect(call("chat.decide", 1000, { id: "a-1" })).rejects.toThrow(
      /may not send id or cmd/,
    );
    await expect(call("chat.send", 1000, { cmd: "chat.stop" })).rejects.toThrow(
      /may not send id or cmd/,
    );
  });

  it("answers commands that leave the envelope alone", async () => {
    installChannel((cmd) => ({ ok: true, echoed: cmd }));

    await expect(
      call("chat.decide", 1000, { approval_id: "a-1", decision: "deny" }),
    ).resolves.toMatchObject({ ok: true, echoed: "chat.decide" });
  });
});
