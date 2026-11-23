// The shared first-open rule, on its own: the strict-false test a page's
// snapshot has to meet before the page pays a read, the failure that must not
// pay one, and the remount that waits for the read already in flight.
import { render, screen, waitFor } from "@testing-library/react";
import { useCallback } from "react";
import { describe, expect, it, vi } from "vitest";
import { useHeldRead } from "./held";

type Answer = { loaded?: boolean };

function Page({
  page,
  answer,
  onLoad,
  onRead,
}: {
  page: string;
  answer: Answer | null;
  onLoad?: () => void;
  onRead: () => Promise<void>;
}) {
  const load = useCallback(async () => {
    onLoad?.();
    return answer;
  }, [answer, onLoad]);
  useHeldRead(page, load, onRead);
  return <p>held</p>;
}

describe("the held-state read", () => {
  it("pays the read when the app says nothing has been read", async () => {
    const onRead = vi.fn(async () => {});
    render(<Page page="fresh" answer={{ loaded: false }} onRead={onRead} />);
    await waitFor(() => expect(onRead).toHaveBeenCalledTimes(1));
  });

  // An answer that does not say is not a claim of emptiness: only a strict
  // `false` reads, so an older answer cannot make a page fetch on every open.
  it.each([{ loaded: true }, {}])("leaves %o alone", async (answer) => {
    const onRead = vi.fn(async () => {});
    render(
      <Page
        page={`quiet-${JSON.stringify(answer)}`}
        answer={answer}
        onRead={onRead}
      />,
    );
    await new Promise((resolve) => setTimeout(resolve, 10));
    expect(onRead).not.toHaveBeenCalled();
  });

  // A refused load already put its sentence on the page; a read on top of it
  // would turn one failure into two.
  it("does not read after a failed load", async () => {
    const onRead = vi.fn(async () => {});
    render(<Page page="failed" answer={null} onRead={onRead} />);
    await new Promise((resolve) => setTimeout(resolve, 10));
    expect(onRead).not.toHaveBeenCalled();
  });

  // The page has to be able to say a read is in progress: an unread answer
  // alone reads as an empty account while the read is still running.
  it("reports reading while the owed read runs", async () => {
    let release = () => {};
    const readInFlight = new Promise<void>((resolve) => {
      release = resolve;
    });
    function Telling() {
      const reading = useHeldRead("telling", async () => ({ loaded: false }), () =>
        readInFlight,
      );
      return <p>{reading ? "reading" : "settled"}</p>;
    }

    render(<Telling />);
    expect(await screen.findByText("reading")).toBeInTheDocument();
    release();
    expect(await screen.findByText("settled")).toBeInTheDocument();
  });

  // A page pressed away and back while its read runs asks an unread snapshot
  // again: it waits for the read in flight -- one read, not two -- and then
  // paints what that read left instead of the sentence it replaced.
  it("waits for the read in flight instead of starting a second", async () => {
    let release = () => {};
    const readInFlight = new Promise<void>((resolve) => {
      release = resolve;
    });
    const onRead = vi.fn(() => readInFlight);
    const first = render(
      <Page page="remount" answer={{ loaded: false }} onRead={onRead} />,
    );
    await waitFor(() => expect(onRead).toHaveBeenCalledTimes(1));

    first.unmount();
    const onLoad = vi.fn();
    render(
      <Page
        page="remount"
        answer={{ loaded: false }}
        onLoad={onLoad}
        onRead={onRead}
      />,
    );
    await waitFor(() => expect(onLoad).toHaveBeenCalledTimes(1));
    expect(onRead).toHaveBeenCalledTimes(1);

    release();
    // The wait is paid off by asking its own snapshot again, which is what
    // paints the rows the read landed.
    await waitFor(() => expect(onLoad).toHaveBeenCalledTimes(2));
    expect(onRead).toHaveBeenCalledTimes(1);

    // That read is over, so a later open may owe its own.
    render(<Page page="remount" answer={{ loaded: false }} onRead={onRead} />);
    await waitFor(() => expect(onRead).toHaveBeenCalledTimes(2));
  });
});
