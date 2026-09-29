// @vitest-environment jsdom
import { beforeEach, afterEach, expect, test, vi } from "vitest";

const { invoke, listen } = vi.hoisted(() => ({
  invoke: vi.fn(),
  listen: vi.fn(),
}));
vi.mock("@tauri-apps/api/core", () => ({ invoke }));
vi.mock("@tauri-apps/api/event", () => ({ listen }));

const disk = (number: number, id: string) => ({
  number,
  selection_id: id,
  name: `USB ${id}`,
  size_bytes: 32e9,
  size_label: "32 GB",
  drive_letters: [],
  ready: true,
  blocked_reason: null,
  blocked_detail: null,
});
let disks: ReturnType<typeof disk>[];
const click = (selector: string) => {
  const button = document.querySelector<HTMLButtonElement>(selector);
  expect(button, selector).not.toBeNull();
  button!.click();
};
const acknowledge = () => {
  const el = document.querySelector<HTMLInputElement>("[data-ack]")!;
  el.checked = true;
  el.dispatchEvent(new Event("change", { bubbles: true }));
};
const settle = async () => {
  for (let i = 0; i < 20; i++) await Promise.resolve();
};

beforeEach(async () => {
  vi.resetModules();
  vi.useFakeTimers();
  vi.clearAllMocks();
  localStorage.clear();
  document.body.innerHTML = '<div id="app"></div>';
  Object.defineProperty(window, "__TAURI_INTERNALS__", {
    value: {},
    configurable: true,
  });
  disks = [disk(2, "original"), disk(3, "target")];
  listen.mockResolvedValue(vi.fn());
  invoke.mockImplementation(async (command: string) => {
    if (command === "list_disks") return { disks: [...disks], notes: [] };
    if (command === "is_simulated") return false;
    if (command === "analyze_source")
      return { bytes: 1e9, size_label: "1 GB", partitions: 1, scheme: "MBR" };
    if (command === "write_image" || command === "clone_disk")
      return new Promise(() => {});
  });
  await import("../src/main");
  await settle();
});
afterEach(() => {
  vi.clearAllTimers();
  vi.useRealTimers();
});

test("burn sends the confirmed selection token and verification choice", async () => {
  click('[data-mode="burn"]');
  await settle();
  click('[data-disk="2"]');
  click('[data-go="2"]');
  click('[data-go="3"]');
  const verify = document.querySelector<HTMLInputElement>("[data-verify]")!;
  verify.checked = true;
  verify.dispatchEvent(new Event("change", { bubbles: true }));
  acknowledge();
  click('[data-go="4"]');
  await settle();
  expect(invoke).toHaveBeenCalledWith("write_image", {
    selectionId: "original",
    loader: "MShell",
    verify: true,
  });
  click("[data-cancel]");
  await settle();
  expect(invoke).toHaveBeenCalledWith("cancel_write");
});

test("replacement using the same disk number cannot change a confirmed request", async () => {
  click('[data-mode="burn"]');
  await settle();
  click('[data-disk="2"]');
  click('[data-go="2"]');
  click('[data-go="3"]');
  disks = [disk(2, "replacement"), disk(3, "target")];
  await vi.advanceTimersByTimeAsync(3500);
  acknowledge();
  click('[data-go="4"]');
  await settle();
  expect(invoke).toHaveBeenCalledWith(
    "write_image",
    expect.objectContaining({ selectionId: "original" }),
  );
});

test("clone preserves both selection tokens and excludes the source from targets", async () => {
  click('[data-mode="clone"]');
  await settle();
  click('[data-disk="2"]');
  expect(document.querySelector('[data-disk="2"]')).toBeNull();
  click('[data-disk="3"]');
  await settle();
  expect(invoke).toHaveBeenCalledWith("analyze_source", {
    selectionId: "original",
  });
  acknowledge();
  click('[data-go="4"]');
  await settle();
  expect(invoke).toHaveBeenCalledWith("clone_disk", {
    source: "original",
    target: "target",
    verify: false,
  });
});

test("a disconnected source returns clone to source selection", async () => {
  click('[data-mode="clone"]');
  await settle();
  click('[data-disk="2"]');
  disks = [disk(2, "replacement"), disk(3, "target")];
  await vi.advanceTimersByTimeAsync(3500);
  expect(document.querySelector('[data-disk="2"]')).not.toBeNull();
  expect(document.querySelector('[data-go="4"]')).toBeNull();
  expect(invoke.mock.calls.some(([name]) => name === "clone_disk")).toBe(false);
});

test("a late source-analysis failure cannot overwrite the screen after Back", async () => {
  let reject!: (e: unknown) => void;
  const normal = invoke.getMockImplementation()!;
  invoke.mockImplementation((command: string, ...args: unknown[]) =>
    command === "analyze_source"
      ? new Promise((_, no) => {
          reject = no;
        })
      : normal(command, ...args),
  );
  click('[data-mode="clone"]');
  await settle();
  click('[data-disk="2"]');
  click('[data-disk="3"]');
  click("[data-back]");
  reject("old analysis failed");
  await settle();
  expect(document.querySelector('[data-disk="3"]')).not.toBeNull();
  expect(document.querySelector(".error-box")).toBeNull();
});

test("destructive action requires explicit acknowledgment", async () => {
  click('[data-mode="burn"]');
  await settle();
  click('[data-disk="2"]');
  click('[data-go="2"]');
  click('[data-go="3"]');
  expect(
    document.querySelector<HTMLButtonElement>('[data-go="4"]')!.disabled,
  ).toBe(true);
  click('[data-go="4"]');
  expect(invoke.mock.calls.some(([name]) => name === "write_image")).toBe(
    false,
  );
  acknowledge();
  expect(
    document.querySelector<HTMLButtonElement>('[data-go="4"]')!.disabled,
  ).toBe(false);
});

test("clone refuses a target smaller than the analyzed source layout", async () => {
  disks[1].size_bytes = 500e6;
  click('[data-mode="clone"]');
  await settle();
  click('[data-disk="2"]');
  click('[data-disk="3"]');
  await settle();
  acknowledge();
  expect(
    document.querySelector<HTMLButtonElement>('[data-go="4"]')!.disabled,
  ).toBe(true);
  click('[data-go="4"]');
  expect(invoke.mock.calls.some(([name]) => name === "clone_disk")).toBe(false);
});

test("failed cancellation allows retry without leaving the running operation", async () => {
  click('[data-mode="burn"]');
  await settle();
  click('[data-disk="2"]');
  click('[data-go="2"]');
  click('[data-go="3"]');
  acknowledge();
  click('[data-go="4"]');
  await settle();
  invoke.mockRejectedValueOnce(new Error("cancel unavailable"));
  click("[data-cancel]");
  await settle();
  expect(
    document.querySelector<HTMLButtonElement>("[data-cancel]")!.disabled,
  ).toBe(false);
  expect(document.querySelector("[data-mode]")).toBeNull();
  click("[data-cancel]");
  await settle();
  expect(
    invoke.mock.calls.filter(([name]) => name === "cancel_write"),
  ).toHaveLength(2);
  expect(
    document.querySelector<HTMLButtonElement>("[data-cancel]")!.disabled,
  ).toBe(true);
});
