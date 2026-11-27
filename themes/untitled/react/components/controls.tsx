// Interactive controls for the React track pages, built on Untitled
// UI React (free tier) primitives. This is the theme's control
// library: pages import from `@theme/components/controls`, never from
// the vendored primitives directly. Props are plain React props
// (onClick, string onChange, ISO date strings) - the React Aria
// conventions (onPress, DateValue) are adapted inside, so page code
// stays simple.
//
// `Table` / `TableCard` are re-exported as-is: they are compound
// components (Table.Head / Table.Row / Table.Cell ...) and pages use
// that structure directly.

import type { ComponentType, HTMLAttributes, ReactNode } from "react";
import { parseDate } from "@internationalized/date";
import { XClose } from "@untitledui/icons";
import { toast as sonnerToast, Toaster as SonnerToaster } from "sonner";
import { Button as UntitledButton } from "./base/buttons/button";
import { Checkbox as UntitledCheckbox } from "./base/checkbox/checkbox";
import { DatePicker as UntitledDatePicker } from "./application/date-picker/date-picker";
import { Input as UntitledInput } from "./base/input/input";
import {
  Dialog as UntitledDialog,
  DialogTrigger as UntitledDialogTrigger,
  Modal as UntitledModal,
  ModalOverlay as UntitledModalOverlay,
} from "./application/modals/modal";
import { PaginationPageDefault as UntitledPagination } from "./application/pagination/pagination";
import { Select as UntitledSelect } from "./base/select/select";
import type { SelectItemType } from "./base/select/select-shared";
import { Tab as UntitledTab, TabList as UntitledTabList, TabPanel as UntitledTabPanel, Tabs as UntitledTabs } from "./application/tabs/tabs";
import { Table as UntitledTable, TableCard as UntitledTableCard } from "./application/table/table";
import { TextArea as UntitledTextArea } from "./base/textarea/textarea";
import { Toggle as UntitledToggle } from "./base/toggle/toggle";
import { Tooltip as UntitledTooltip } from "./base/tooltip/tooltip";

// Button

const buttonColors = {
  primary: "primary",
  secondary: "secondary",
  tertiary: "tertiary",
  destructive: "primary-destructive",
  ghost: "tertiary",
  link: "link-gray",
} as const;

export function Button({
  color = "primary",
  size = "sm",
  isDisabled,
  isLoading,
  iconLeading,
  iconTrailing,
  onClick,
  onPress,
  type = "button",
  className,
  ariaLabel,
  ariaExpanded,
  children,
}: {
  color?: keyof typeof buttonColors;
  size?: "xs" | "sm" | "md" | "lg";
  isDisabled?: boolean;
  isLoading?: boolean;
  iconLeading?: ReactNode;
  iconTrailing?: ReactNode;
  onClick?: () => void;
  onPress?: () => void;
  type?: "button" | "submit" | "reset";
  className?: string;
  ariaLabel?: string;
  ariaExpanded?: boolean;
  children?: ReactNode;
}) {
  return (
    <UntitledButton
      color={buttonColors[color]}
      size={size}
      isDisabled={isDisabled}
      isLoading={isLoading}
      iconLeading={iconLeading}
      iconTrailing={iconTrailing}
      onPress={onPress ?? onClick}
      type={type}
      className={className}
      aria-label={ariaLabel}
      aria-expanded={ariaExpanded}
    >
      {children}
    </UntitledButton>
  );
}

// Input / TextArea

export function Input({
  label,
  hint,
  placeholder,
  value,
  defaultValue,
  onChange,
  type = "text",
  isDisabled,
  isInvalid,
  isRequired,
  size = "md",
  icon,
  className,
  id,
  autoComplete,
  inputMode,
  maxLength,
  autoFocus,
}: {
  label?: string;
  hint?: ReactNode;
  placeholder?: string;
  value?: string;
  defaultValue?: string;
  onChange?: (value: string) => void;
  type?: string;
  isDisabled?: boolean;
  isInvalid?: boolean;
  isRequired?: boolean;
  size?: "sm" | "md" | "lg";
  /** Leading icon component (the vendored Input takes a component, not an element). */
  icon?: ComponentType<HTMLAttributes<HTMLOrSVGElement>>;
  className?: string;
  id?: string;
  autoComplete?: string;
  inputMode?: "text" | "numeric" | "decimal" | "email" | "search" | "tel" | "url" | "none";
  maxLength?: number;
  autoFocus?: boolean;
}) {
  return (
    <UntitledInput
      label={label}
      hint={hint}
      placeholder={placeholder}
      value={value}
      defaultValue={defaultValue}
      onChange={onChange}
      type={type}
      isDisabled={isDisabled}
      isInvalid={isInvalid}
      isRequired={isRequired}
      size={size}
      icon={icon}
      className={className}
      id={id}
      autoComplete={autoComplete}
      inputMode={inputMode}
      maxLength={maxLength}
      autoFocus={autoFocus}
    />
  );
}

export function TextArea({
  label,
  hint,
  placeholder,
  value,
  defaultValue,
  onChange,
  rows,
  isDisabled,
  isInvalid,
  isRequired,
  size = "md",
  className,
}: {
  label?: string;
  hint?: ReactNode;
  placeholder?: string;
  value?: string;
  defaultValue?: string;
  onChange?: (value: string) => void;
  rows?: number;
  isDisabled?: boolean;
  isInvalid?: boolean;
  isRequired?: boolean;
  size?: "sm" | "md";
  className?: string;
}) {
  return (
    <UntitledTextArea
      label={label}
      hint={hint}
      placeholder={placeholder}
      value={value}
      defaultValue={defaultValue}
      onChange={onChange}
      rows={rows}
      isDisabled={isDisabled}
      isInvalid={isInvalid}
      isRequired={isRequired}
      size={size}
      className={className}
    />
  );
}

// Select

export type SelectOption = {
  id: string;
  label: string;
  isDisabled?: boolean;
};

export function Select({
  label,
  hint,
  placeholder,
  items,
  selectedKey,
  defaultSelectedKey,
  onSelectionChange,
  isDisabled,
  size = "md",
  className,
  ariaLabel,
}: {
  label?: string;
  hint?: string;
  placeholder?: string;
  items: SelectOption[];
  selectedKey?: string | null;
  defaultSelectedKey?: string;
  onSelectionChange?: (key: string) => void;
  isDisabled?: boolean;
  size?: "sm" | "md" | "lg";
  className?: string;
  ariaLabel?: string;
}) {
  const ariaItems: SelectItemType[] = items.map((item) => ({
    id: item.id,
    label: item.label,
    isDisabled: item.isDisabled,
  }));
  return (
    <UntitledSelect
      label={label}
      hint={hint}
      placeholder={placeholder}
      items={ariaItems}
      selectedKey={selectedKey ?? undefined}
      defaultSelectedKey={defaultSelectedKey}
      onSelectionChange={(key) => {
        if (typeof key === "string") onSelectionChange?.(key);
      }}
      isDisabled={isDisabled}
      size={size}
      className={className}
      aria-label={ariaLabel}
    >
      {(item) => (
        <UntitledSelect.Item key={item.id} id={item.id} label={item.label} />
      )}
    </UntitledSelect>
  );
}

// Checkbox / Switch

export function Checkbox({
  label,
  hint,
  isSelected,
  defaultSelected,
  onChange,
  isDisabled,
  isIndeterminate,
  size = "sm",
  className,
}: {
  label?: ReactNode;
  hint?: ReactNode;
  isSelected?: boolean;
  defaultSelected?: boolean;
  onChange?: (selected: boolean) => void;
  isDisabled?: boolean;
  isIndeterminate?: boolean;
  size?: "sm" | "md";
  className?: string;
}) {
  return (
    <UntitledCheckbox
      label={label}
      hint={hint}
      isSelected={isSelected}
      defaultSelected={defaultSelected}
      onChange={onChange}
      isDisabled={isDisabled}
      isIndeterminate={isIndeterminate}
      size={size}
      className={className}
    />
  );
}

export function Switch({
  label,
  hint,
  isSelected,
  defaultSelected,
  onChange,
  isDisabled,
  size = "sm",
  className,
}: {
  label?: string;
  hint?: ReactNode;
  isSelected?: boolean;
  defaultSelected?: boolean;
  onChange?: (selected: boolean) => void;
  isDisabled?: boolean;
  size?: "sm" | "md";
  className?: string;
}) {
  return (
    <UntitledToggle
      label={label}
      hint={hint}
      isSelected={isSelected}
      defaultSelected={defaultSelected}
      onChange={onChange}
      isDisabled={isDisabled}
      size={size}
      className={className}
    />
  );
}

// Tabs

export type TabItem = {
  id: string;
  label: string;
  badge?: string;
};

export function Tabs({
  items,
  selectedKey,
  onSelectionChange,
  children,
  className,
}: {
  items: TabItem[];
  selectedKey?: string;
  onSelectionChange?: (key: string) => void;
  children: ReactNode;
  className?: string;
}) {
  return (
    <UntitledTabs
      selectedKey={selectedKey}
      onSelectionChange={(key) => {
        if (typeof key === "string") onSelectionChange?.(key);
      }}
      className={className}
    >
      <UntitledTabList>
        {items.map((item) => (
          <UntitledTab key={item.id} id={item.id} label={item.label} badge={item.badge}>
            {item.label}
          </UntitledTab>
        ))}
      </UntitledTabList>
      {children}
    </UntitledTabs>
  );
}

export const TabPanel = UntitledTabPanel;

// Table

export const Table = UntitledTable;
export const TableCard = UntitledTableCard;

// Pagination

export function Pagination({
  page,
  total,
  onPageChange,
  className,
}: {
  page: number;
  total: number;
  onPageChange?: (page: number) => void;
  className?: string;
}) {
  return (
    <UntitledPagination
      page={page}
      total={total}
      onPageChange={onPageChange}
      className={className}
    />
  );
}

// Dialog

export function Dialog({
  isOpen,
  onOpenChange,
  title,
  children,
  footer,
}: {
  isOpen: boolean;
  onOpenChange?: (open: boolean) => void;
  title?: string;
  children: ReactNode;
  footer?: ReactNode;
}) {
  return (
    <UntitledModalOverlay
      isOpen={isOpen}
      onOpenChange={onOpenChange}
      isDismissable
    >
      <UntitledModal>
        <UntitledDialog>
          {({ close }) => (
            <div className="flex flex-col">
              {title !== undefined && (
                <div className="flex items-start justify-between gap-3 border-b border-secondary px-5 py-4">
                  <h2 className="text-md font-semibold text-primary">{title}</h2>
                  <button
                    type="button"
                    aria-label="Close dialog"
                    onClick={() => close()}
                    className="cursor-pointer rounded-md p-1 text-tertiary hover:bg-primary_hover hover:text-primary"
                  >
                    <XClose className="size-5" />
                  </button>
                </div>
              )}
              <div className="px-5 py-4">{children}</div>
              {footer !== undefined && (
                <div className="flex justify-end gap-2 border-t border-secondary px-5 py-4">
                  {footer}
                </div>
              )}
            </div>
          )}
        </UntitledDialog>
      </UntitledModal>
    </UntitledModalOverlay>
  );
}

export const DialogTrigger = UntitledDialogTrigger;

// DatePicker (ISO date strings in, ISO date strings out)

export function DatePicker({
  label,
  value,
  defaultValue,
  onChange,
  isDisabled,
  size = "sm",
  className,
}: {
  label?: string;
  value?: string | null;
  defaultValue?: string;
  onChange?: (iso: string | null) => void;
  isDisabled?: boolean;
  size?: "sm" | "md" | "lg";
  className?: string;
}) {
  return (
    <UntitledDatePicker
      aria-label={label ?? "Choose a date"}
      value={value ? parseDate(value) : null}
      defaultValue={defaultValue ? parseDate(defaultValue) : undefined}
      onChange={(next) => onChange?.(next ? next.toString() : null)}
      isDisabled={isDisabled}
      size={size}
      className={className}
    />
  );
}

// Tooltip

export function Tooltip({
  title,
  description,
  children,
}: {
  title: ReactNode;
  description?: ReactNode;
  children: ReactNode;
}) {
  return (
    <UntitledTooltip title={title} description={description}>
      {children}
    </UntitledTooltip>
  );
}

// Toast (sonner). Mount <Toaster /> once near the app root; call
// toast.success / toast.error / toast.info from anywhere.

export const toast = sonnerToast;

export function Toaster() {
  const dark =
    typeof document !== "undefined" &&
    document.documentElement.classList.contains("dark");
  return (
    <SonnerToaster
      position="top-right"
      theme={dark ? "dark" : "light"}
      closeButton
      richColors
    />
  );
}
