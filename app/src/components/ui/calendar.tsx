"use client";

import { ChevronLeft, ChevronRight } from "lucide-react";
import { DayPicker, type DayPickerProps } from "react-day-picker";
import "react-day-picker/style.css";

import { cn } from "../../lib/utils";
import { buttonVariants } from "./button";

/**
 * Thin shadcn-flavoured wrapper around `react-day-picker` so the App
 * home page can pick a scan-since timestamp with a calendar instead
 * of hand-typing a Y/M/D. Locale defaults to zh-CN (星期X 月X) via
 * the `locale` prop supplied by the caller — Calendar itself does
 * not pin a locale.
 */
export type CalendarProps = DayPickerProps & {
  className?: string;
};

export function Calendar({
  className,
  classNames,
  showOutsideDays = true,
  ...props
}: CalendarProps) {
  return (
    <DayPicker
      showOutsideDays={showOutsideDays}
      className={cn("p-3", className)}
      classNames={{
        months: "flex flex-col sm:flex-row gap-2",
        month: "flex flex-col gap-4",
        month_caption: "flex justify-center pt-1 relative items-center h-8",
        caption_label: "text-sm font-medium",
        nav: "flex items-center gap-1 absolute inset-x-0 top-0 justify-between",
        button_previous: cn(
          buttonVariants({ variant: "outline" }),
          "h-7 w-7 bg-transparent p-0 opacity-50 hover:opacity-100"
        ),
        button_next: cn(
          buttonVariants({ variant: "outline" }),
          "h-7 w-7 bg-transparent p-0 opacity-50 hover:opacity-100"
        ),
        month_grid: "w-full border-collapse space-y-1",
        weekdays: "flex",
        weekday: "text-neutral-500 rounded-md w-8 font-normal text-[0.8rem] flex-1",
        week: "flex w-full mt-1",
        day: cn(
          "relative p-0 text-center text-sm focus-within:relative focus-within:z-20 [&:has([aria-selected])]:bg-neutral-100 [&:has([aria-selected].day-range-end)]:rounded-r-md [&:has([aria-selected].day-outside)]:rounded-r-md [&:has([aria-selected].day-range-start)]:rounded-l-md flex-1",
          classNames?.day
        ),
        day_button: cn(
          buttonVariants({ variant: "ghost" }),
          "h-8 w-8 p-0 font-normal aria-selected:opacity-100"
        ),
        range_start: "day-range-start",
        range_end: "day-range-end",
        selected:
          "bg-neutral-900 text-neutral-50 hover:bg-neutral-900 hover:text-neutral-50 focus:bg-neutral-900 focus:text-neutral-50",
        today: "bg-neutral-100 text-neutral-900",
        outside:
          "day-outside text-neutral-400 aria-selected:bg-neutral-100/50 aria-selected:text-neutral-400",
        disabled: "text-neutral-400 opacity-50",
        range_middle:
          "aria-selected:bg-neutral-100 aria-selected:text-neutral-900",
        hidden: "invisible",
        ...classNames,
      }}
      components={{
        Chevron: ({ orientation }) => {
          const Icon =
            orientation === "left" ? ChevronLeft : ChevronRight;
          return <Icon className="h-4 w-4" aria-hidden />;
        },
      }}
      {...props}
    />
  );
}
