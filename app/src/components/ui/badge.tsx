import * as React from "react";
import { cva, type VariantProps } from "class-variance-authority";
import { cn } from "../../lib/utils";

const badgeVariants = cva(
  "inline-flex items-center rounded-md border px-2 py-0.5 text-[11px] font-medium transition-colors",
  {
    variants: {
      variant: {
        default: "border-neutral-200 bg-neutral-50 text-neutral-700",
        codex: "border-emerald-200 bg-emerald-50 text-emerald-800",
        claude: "border-orange-200 bg-orange-50 text-orange-800",
        opencode: "border-sky-200 bg-sky-50 text-sky-800",
        workbuddy: "border-pink-200 bg-pink-50 text-pink-800",
        doubao: "border-rose-200 bg-rose-50 text-rose-800",
        doubao_work: "border-rose-300 bg-rose-100 text-rose-900",
        minimax_code: "border-violet-200 bg-violet-50 text-violet-800",
        trae: "border-yellow-200 bg-yellow-50 text-yellow-800",
        codebuddy_cn: "border-amber-200 bg-amber-50 text-amber-800",
        success: "border-emerald-200 bg-emerald-50 text-emerald-800",
        warning: "border-amber-200 bg-amber-50 text-amber-800",
        danger: "border-red-200 bg-red-50 text-red-800",
        outline: "border-neutral-300 bg-white text-neutral-700",
      },
    },
    defaultVariants: { variant: "default" },
  }
);

export interface BadgeProps
  extends React.HTMLAttributes<HTMLSpanElement>,
    VariantProps<typeof badgeVariants> {}

export function Badge({ className, variant, ...props }: BadgeProps) {
  return (
    <span className={cn(badgeVariants({ variant }), className)} {...props} />
  );
}