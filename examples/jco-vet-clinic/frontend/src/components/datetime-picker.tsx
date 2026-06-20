import { useState } from "react"
import { CalendarIcon } from "lucide-react"
import { Button } from "@/components/ui/button"
import { Calendar } from "@/components/ui/calendar"
import { Input } from "@/components/ui/input"
import { Label } from "@/components/ui/label"
import { Popover, PopoverContent, PopoverTrigger } from "@/components/ui/popover"
import { cn } from "@/lib/utils"

/**
 * A date + time picker that emits a `YYYY-MM-DDTHH:mm` string (the format the
 * backend's validate rules + domain expect). Calendar (shadcn / react-day-picker)
 * for the day, a native time input for the clock — kept dependency-light.
 */
export function DateTimePicker({
  value,
  onChange,
}: {
  value: string
  onChange: (next: string) => void
}) {
  const [open, setOpen] = useState(false)

  // split the controlled "YYYY-MM-DDTHH:mm" value into its parts
  const [datePart, timePart] = value ? value.split("T") : ["", ""]
  const selected = datePart ? new Date(`${datePart}T00:00:00`) : undefined
  const time = timePart || "10:00"

  function fmtDate(d: Date): string {
    const y = d.getFullYear()
    const m = String(d.getMonth() + 1).padStart(2, "0")
    const day = String(d.getDate()).padStart(2, "0")
    return `${y}-${m}-${day}`
  }

  function pickDay(d: Date | undefined) {
    if (!d) return
    onChange(`${fmtDate(d)}T${time}`)
    setOpen(false)
  }

  function pickTime(t: string) {
    const day = datePart || fmtDate(new Date())
    onChange(`${day}T${t || "10:00"}`)
  }

  const label = datePart
    ? new Date(`${datePart}T${time}`).toLocaleString(undefined, {
        dateStyle: "medium",
        timeStyle: "short",
      })
    : "Pick a date & time"

  return (
    <div className="flex items-end gap-2">
      <div className="space-y-2">
        <Label>When</Label>
        <Popover open={open} onOpenChange={setOpen}>
          <PopoverTrigger asChild>
            <Button
              type="button"
              variant="outline"
              className={cn("w-56 justify-start font-normal", !datePart && "text-muted-foreground")}
            >
              <CalendarIcon className="mr-2 h-4 w-4" />
              {label}
            </Button>
          </PopoverTrigger>
          <PopoverContent className="w-auto p-0" align="start">
            <Calendar
              mode="single"
              selected={selected}
              onSelect={pickDay}
              autoFocus
            />
          </PopoverContent>
        </Popover>
      </div>
      <div className="space-y-2">
        <Label htmlFor="appt-time">Time</Label>
        <Input
          id="appt-time"
          type="time"
          className="w-32"
          value={time}
          onChange={(e) => pickTime(e.target.value)}
        />
      </div>
    </div>
  )
}
