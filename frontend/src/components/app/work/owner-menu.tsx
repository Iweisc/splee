"use client";

import { useEffect, useLayoutEffect, useRef, useState } from "react";
import { Check, ChevronDown, UserPlus, UserRound } from "lucide-react";
import useSWR from "swr";
import { Select } from "@/components/ui/form";
import { Avatar } from "@/components/ui/misc";
import { fetcher, useMe } from "@/hooks/use-me";
import { api } from "@/lib/api";
import type { Me, Member, OwnerFilter, OwnerView } from "@/lib/types";
import { cn } from "@/lib/utils";
import { toastError } from "./states";

export function useMembers() {
  return useSWR<Member[]>("/organization/users", fetcher).data;
}

export const memberName = (m: Pick<Member, "name" | "email">) => m.name || m.email;

/**
 * Picks who owns a lead from the team, or nobody. A menu drawn by the page rather than a native select, so it can show
 * each member's Away and fit a pipeline card as an avatar. `look` is the button: a select-like field, an avatar, or a
 * bulk action that always reads "Assign to".
 */
export function OwnerMenu({
  ownerId,
  ownerName,
  onPick,
  look = "field",
  disabled,
  className,
}: {
  ownerId: string | null;
  /** Shown before the team loads, and for an owner who has left it. */
  ownerName?: string | null;
  onPick: (ownerId: string | null) => void;
  look?: "field" | "avatar" | "bulk";
  disabled?: boolean;
  className?: string;
}) {
  const members = useMembers();
  const [open, setOpen] = useState(false);
  const button = useRef<HTMLButtonElement>(null);
  const menu = useRef<HTMLDivElement>(null);
  const [place, setPlace] = useState<{ top: number; left: number } | null>(null);
  const owner = members?.find((m) => m.user_id === ownerId);
  const name = owner ? memberName(owner) : ownerId ? ownerName : null;

  // Fixed to the button rather than inside its box, so a pipeline column or table that scrolls doesn't clip it. It opens
  // upwards when there's no room below, and keeps inside the screen sideways.
  useLayoutEffect(() => {
    if (!open || !button.current || !menu.current) return;
    const b = button.current.getBoundingClientRect();
    const m = menu.current.getBoundingClientRect();
    const below = b.bottom + 4 + m.height <= window.innerHeight - 8;
    const top = below ? b.bottom + 4 : Math.max(8, b.top - 4 - m.height);
    const left = Math.min(Math.max(8, look === "field" ? b.left : b.right - m.width), window.innerWidth - m.width - 8);
    setPlace({ top, left });
  }, [open, look, members]);

  useEffect(() => {
    if (!open) return;
    menu.current?.querySelector<HTMLElement>("[aria-checked=true]")?.focus();
    const press = (e: PointerEvent) => {
      if (!menu.current?.contains(e.target as Node) && !button.current?.contains(e.target as Node)) setOpen(false);
    };
    const key = (e: KeyboardEvent) => {
      if (e.key !== "Escape") return;
      // Marked handled, so a lead window around it stays open. Capture runs this before the window's own listener.
      e.preventDefault();
      e.stopPropagation();
      setOpen(false);
      button.current?.focus();
    };
    const away = () => setOpen(false);
    document.addEventListener("pointerdown", press);
    document.addEventListener("keydown", key, true);
    window.addEventListener("resize", away);
    window.addEventListener("scroll", away, true);
    return () => {
      document.removeEventListener("pointerdown", press);
      document.removeEventListener("keydown", key, true);
      window.removeEventListener("resize", away);
      window.removeEventListener("scroll", away, true);
    };
  }, [open]);

  const pick = (id: string | null) => {
    setOpen(false);
    button.current?.focus();
    if (id !== ownerId || look === "bulk") onPick(id);
  };

  const moveFocus = (e: React.KeyboardEvent) => {
    if (e.key !== "ArrowDown" && e.key !== "ArrowUp") return;
    e.preventDefault();
    const items = [...(menu.current?.querySelectorAll<HTMLElement>("[role=menuitemradio]") ?? [])];
    const at = items.indexOf(document.activeElement as HTMLElement);
    items[(at + (e.key === "ArrowDown" ? 1 : items.length - 1)) % items.length]?.focus();
  };

  return (
    <>
      <button
        ref={button}
        type="button"
        disabled={disabled}
        onClick={(e) => {
          // A pipeline card opens its deal on click, so the menu keeps its own.
          e.stopPropagation();
          setPlace(null);
          setOpen((o) => !o);
        }}
        aria-haspopup="menu"
        aria-expanded={open}
        aria-label={look === "bulk" ? undefined : name ? `Owner: ${name}. Change the owner` : "No owner. Assign it"}
        title={look === "avatar" ? (name ?? "Unassigned") : undefined}
        className={cn(
          "inline-flex shrink-0 items-center text-sm disabled:opacity-50",
          look === "field" && "h-8 max-w-full gap-2 rounded-lg border border-border-strong bg-card pr-2 pl-1.5 hover:bg-accent",
          look === "bulk" && "h-9 justify-center gap-1.5 rounded-lg border border-border-strong bg-card px-3 font-medium hover:bg-accent",
          look === "avatar" && "rounded-full hover:ring-2 hover:ring-black/10",
          className,
        )}
      >
        {look === "bulk" ? (
          <>
            <UserPlus className="size-3.5" /> Assign to…
          </>
        ) : look === "avatar" ? (
          name ? (
            <Avatar name={name} className="size-5 text-[9px]" />
          ) : (
            <span className="inline-flex size-5 items-center justify-center rounded-full border border-dashed border-foreground/30 text-muted-foreground">
              <UserRound className="size-3" />
            </span>
          )
        ) : (
          <>
            {name ? <Avatar name={name} className="size-5 text-[9px]" /> : <UserRound className="size-4 text-muted-foreground" />}
            <span data-owner-name className={cn("truncate", !name && "text-muted-foreground")}>
              {name ?? "Unassigned"}
            </span>
            <ChevronDown className="size-3.5 text-muted-foreground" />
          </>
        )}
      </button>
      {open && (
        <div
          ref={menu}
          role="menu"
          aria-label="Owner"
          onKeyDown={moveFocus}
          onClick={(e) => e.stopPropagation()}
          style={place ?? { top: 0, left: 0, visibility: "hidden" }}
          className="fixed z-50 max-h-80 w-60 overflow-y-auto rounded-xl bg-card p-1 text-sm shadow-pop"
        >
          <Item checked={look !== "bulk" && !ownerId} onClick={() => pick(null)}>
            <UserRound className="size-4 text-muted-foreground" />
            <span className="flex-1">Unassigned</span>
          </Item>
          {members?.map((m) => (
            <Item key={m.user_id} checked={look !== "bulk" && m.user_id === ownerId} onClick={() => pick(m.user_id)}>
              <Avatar name={memberName(m)} className="size-5 text-[9px]" />
              <span className="min-w-0 flex-1 truncate">{memberName(m)}</span>
              {m.away && <span className="text-xs text-muted-foreground">Away</span>}
            </Item>
          ))}
        </div>
      )}
    </>
  );
}

function Item({ checked, onClick, children }: { checked: boolean; onClick: () => void; children: React.ReactNode }) {
  return (
    <button
      type="button"
      role="menuitemradio"
      aria-checked={checked}
      onClick={onClick}
      className="flex w-full items-center gap-2 rounded-lg px-2 py-1.5 text-left outline-none hover:bg-accent focus-visible:bg-accent"
    >
      {children}
      <Check className={cn("size-3.5 shrink-0", !checked && "invisible")} />
    </button>
  );
}

/**
 * The Everyone, Mine or Unassigned filter of `view`, remembered on the server for this member, so it's the same on their other
 * browsers. It shows the change at once and saves it behind.
 */
export function useOwnerFilter(view: OwnerView): [OwnerFilter, (f: OwnerFilter) => void] {
  const { me, mutate } = useMe();
  const value = me?.owner_filters[view] ?? "everyone";
  const set = (filter: OwnerFilter) => {
    if (!me) return;
    void mutate(api<Me>("/auth/me", { method: "PATCH", body: { view, owner_filter: filter } }), {
      optimisticData: { ...me, owner_filters: { ...me.owner_filters, [view]: filter } },
      revalidate: false,
    }).catch(toastError);
  };
  return [value, set];
}

/** The owner filter as a select, like the pages' other filters. */
export function OwnerFilterSelect({ value, onChange, className }: { value: OwnerFilter; onChange: (f: OwnerFilter) => void; className?: string }) {
  return (
    <Select value={value} onChange={(e) => onChange(e.target.value as OwnerFilter)} className={cn("h-9 w-auto", className)} aria-label="Owner">
      <option value="everyone">Everyone</option>
      <option value="mine">Mine</option>
      <option value="unassigned">Unassigned</option>
    </Select>
  );
}
