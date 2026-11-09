import { Children, isValidElement } from "react";
import Link from "next/link";
import { Loader2 } from "lucide-react";
import { cn } from "@/lib/utils";

const variants = {
  primary: "bg-primary text-primary-foreground hover:bg-black/85",
  brand: "bg-brand-600 text-white hover:bg-brand-700",
  outline: "border border-border-strong bg-card text-foreground hover:bg-accent",
  ghost: "text-foreground hover:bg-accent",
  danger: "bg-danger text-white hover:bg-red-700",
  link: "text-foreground underline-offset-4 hover:underline px-0",
};

const sizes = {
  sm: "h-8 px-3 text-sm gap-1.5 rounded-lg",
  md: "h-10 px-4 text-sm gap-2 rounded-lg",
  lg: "h-12 px-6 text-base gap-2 rounded-xl",
  icon: "h-9 w-9 rounded-lg",
};

export type ButtonVariant = keyof typeof variants;
export type ButtonSize = keyof typeof sizes;

export function buttonClass(variant: ButtonVariant = "primary", size: ButtonSize = "md", className?: string) {
  return cn(
    "inline-flex items-center justify-center font-medium whitespace-nowrap transition-colors disabled:pointer-events-none disabled:opacity-50 focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-brand-600",
    variants[variant],
    sizes[size],
    className,
  );
}

type ButtonProps = React.ButtonHTMLAttributes<HTMLButtonElement> & {
  variant?: ButtonVariant;
  size?: ButtonSize;
  loading?: boolean;
};

export function Button({ variant, size, loading, className, children, disabled, ...props }: ButtonProps) {
  // While loading, the spinner takes the place and size of a leading icon, so the button keeps its width and never shows two.
  const [first, ...rest] = Children.toArray(children);
  const icon = isValidElement<{ className?: string }>(first) && typeof first.type !== "string" ? first : null;
  return (
    <button className={buttonClass(variant, size, className)} disabled={disabled || loading} {...props}>
      {loading && <Loader2 className={cn("size-4", icon?.props.className, "animate-spin")} />}
      {loading && icon ? rest : children}
    </button>
  );
}

type LinkButtonProps = React.ComponentProps<typeof Link> & { variant?: ButtonVariant; size?: ButtonSize };

export function LinkButton({ variant, size, className, ...props }: LinkButtonProps) {
  return <Link className={buttonClass(variant, size, className)} {...props} />;
}
