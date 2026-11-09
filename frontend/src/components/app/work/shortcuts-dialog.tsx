"use client";

import { useState } from "react";
import { Modal } from "@/components/ui/misc";
import { Kbd, useShortcut } from "./list-keys";

// Keep in sync with the useShortcut and useListKeys calls. The hints under the lists say where each one works.
const shortcuts = (mod: string): { keys: string[]; what: string }[] => [
  { keys: ["j", "k"], what: "Next and previous reply or email" },
  { keys: ["/"], what: "Search the list" },
  { keys: ["e"], what: "Mark the open reply done" },
  { keys: ["r"], what: "Write a reply" },
  { keys: [mod, "Enter"], what: "Send the reply" },
  { keys: ["?"], what: "Show this list" },
];

/** ? lists every keyboard shortcut in the app. Mounted once, in the app shell. */
export function ShortcutsDialog() {
  const [open, setOpen] = useState(false);
  useShortcut("?", () => setOpen(true));
  return (
    <Modal open={open} onClose={() => setOpen(false)} title="Keyboard shortcuts" className="max-w-sm">
      <ShortcutList />
    </Modal>
  );
}

/** Modal only renders this while open, in the browser, so reading navigator can't differ from the server render. */
function ShortcutList() {
  const mod = /Mac|iPhone|iPad/.test(navigator.platform) ? "⌘" : "Ctrl";
  return (
    <dl className="divide-y divide-border text-sm">
      {shortcuts(mod).map((s) => (
        <div key={s.what} className="flex items-center justify-between gap-4 py-2">
          <dt className="text-muted-foreground">{s.what}</dt>
          <dd className="flex shrink-0 gap-1">
            {s.keys.map((k) => (
              <Kbd key={k}>{k}</Kbd>
            ))}
          </dd>
        </div>
      ))}
    </dl>
  );
}
