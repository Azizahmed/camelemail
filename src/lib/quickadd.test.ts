import { describe, expect, it } from "vitest";
import { parseQuickAdd } from "./quickadd";

// Monday 19 October 2026, 10:00 local time.
const NOW = new Date(2026, 9, 19, 10, 0);
const at = (month: number, day: number, h = 0, m = 0, year = 2026) => new Date(year, month, day, h, m);

describe("parseQuickAdd", () => {
  it("reads a day and a time", () => {
    const p = parseQuickAdd("Lunch with Sara tomorrow 1pm", NOW);
    expect(p.title).toBe("Lunch with Sara");
    expect(p.start).toEqual(at(9, 20, 13));
    expect(p.end).toEqual(at(9, 20, 14));
    expect(p.allDay).toBe(false);
    expect(p.rrule).toBeNull();
  });

  it("repeats every weekday", () => {
    const p = parseQuickAdd("Standup every weekday 9:30", NOW);
    expect(p.title).toBe("Standup");
    expect(p.rrule).toBe("FREQ=WEEKLY;BYDAY=MO,TU,WE,TH,FR");
    expect(p.start).toEqual(at(9, 19, 9, 30));
    expect(p.end).toEqual(at(9, 19, 10, 30));
  });

  it("repeats on a named weekday from the coming one", () => {
    const p = parseQuickAdd("Review every friday at 4pm", NOW);
    expect(p.rrule).toBe("FREQ=WEEKLY;BYDAY=FR");
    expect(p.start).toEqual(at(9, 23, 16));
  });

  it("keeps a leading repeat word in the title", () => {
    const p = parseQuickAdd("Weekly review friday 4pm", NOW);
    expect(p.title).toBe("Weekly review");
    expect(p.rrule).toBeNull();
    expect(p.start).toEqual(at(9, 23, 16));
  });

  it("takes a trailing repeat word", () => {
    const p = parseQuickAdd("Gym daily 7am", NOW);
    expect(p.title).toBe("Gym");
    expect(p.rrule).toBe("FREQ=DAILY");
    expect(p.start).toEqual(at(9, 19, 7));
  });

  it("reads 'at 3' without am/pm as the afternoon", () => {
    const p = parseQuickAdd("Dentist friday at 3", NOW);
    expect(p.title).toBe("Dentist");
    expect(p.start).toEqual(at(9, 23, 15));
  });

  it("names today's weekday as next week's", () => {
    const p = parseQuickAdd("Team sync monday", NOW);
    expect(p.allDay).toBe(true);
    expect(p.start).toEqual(at(9, 26));
    expect(p.end).toEqual(at(9, 27));
  });

  it("puts tonight at 19:00 and 'tonight at 9' in the evening", () => {
    expect(parseQuickAdd("Party tonight", NOW).start).toEqual(at(9, 19, 19));
    expect(parseQuickAdd("Dinner tonight at 9", NOW).start).toEqual(at(9, 19, 21));
  });

  it("reads noon and midnight", () => {
    expect(parseQuickAdd("Lunch at noon", NOW).start).toEqual(at(9, 19, 12));
    expect(parseQuickAdd("Release tomorrow midnight", NOW).start).toEqual(at(9, 20, 0));
  });

  it("reads dates in both orders, rolling past dates into next year", () => {
    expect(parseQuickAdd("Conference 3 dec", NOW).start).toEqual(at(11, 3));
    expect(parseQuickAdd("Trip jan 5", NOW).start).toEqual(at(0, 5, 0, 0, 2027));
    expect(parseQuickAdd("Trip jan 5 2026", NOW).start).toEqual(at(0, 5, 0, 0, 2026));
    expect(parseQuickAdd("Exam on 21st October", NOW).start).toEqual(at(9, 21));
  });

  it("reads relative days", () => {
    expect(parseQuickAdd("Follow up in 3 days", NOW).start).toEqual(at(9, 22));
    expect(parseQuickAdd("Follow up in 2 weeks", NOW).start).toEqual(at(10, 2));
    expect(parseQuickAdd("Visit day after tomorrow", NOW).start).toEqual(at(9, 21));
  });

  it("reads time ranges, sharing a trailing am/pm", () => {
    const p = parseQuickAdd("Call 3-4pm", NOW);
    expect(p.title).toBe("Call");
    expect(p.start).toEqual(at(9, 19, 15));
    expect(p.end).toEqual(at(9, 19, 16));
    const q = parseQuickAdd("Workshop 11am to 1pm", NOW);
    expect(q.start).toEqual(at(9, 19, 11));
    expect(q.end).toEqual(at(9, 19, 13));
  });

  it("ends a range that crosses midnight on the next day", () => {
    const p = parseQuickAdd("Late shift 11pm-2am", NOW);
    expect(p.start).toEqual(at(9, 19, 23));
    expect(p.end).toEqual(at(9, 20, 2));
  });

  it("reads a duration", () => {
    const p = parseQuickAdd("Review for 30 min tomorrow at 9am", NOW);
    expect(p.start).toEqual(at(9, 20, 9));
    expect(p.end).toEqual(at(9, 20, 9, 30));
    expect(parseQuickAdd("Hike for 2.5 hours tomorrow 8am", NOW).end).toEqual(at(9, 20, 10, 30));
  });

  it("uses the clicked day when only a time is given", () => {
    const p = parseQuickAdd("Call at 3pm", NOW, at(9, 25, 8));
    expect(p.start).toEqual(at(9, 25, 15));
  });

  it("leaves text without a date or time alone", () => {
    const p = parseQuickAdd("Buy milk", NOW);
    expect(p).toMatchObject({ title: "Buy milk", start: null, end: null, allDay: false, rrule: null, hint: "" });
  });

  it("ignores impossible clock times", () => {
    const p = parseQuickAdd("Call at 25", NOW);
    expect(p.start).toBeNull();
    expect(p.title).toBe("Call at 25");
  });

  it("keeps Arabic titles", () => {
    const p = parseQuickAdd("اجتماع الفريق tomorrow 10am", NOW);
    expect(p.title).toBe("اجتماع الفريق");
    expect(p.start).toEqual(at(9, 20, 10));
  });
});

describe("parseQuickAdd edge cases", () => {
  it("starts a weekday series on a weekday when typed at the weekend", () => {
    const saturday = new Date(2026, 9, 24, 10, 0);
    const p = parseQuickAdd("Standup every weekday 9:30", saturday);
    expect(p.start).toEqual(at(9, 26, 9, 30));
  });

  it("gives a range's start the other half of the day when the shared am/pm would put it after the end", () => {
    const p = parseQuickAdd("Meeting 11-1pm", NOW);
    expect(p.start).toEqual(at(9, 19, 11));
    expect(p.end).toEqual(at(9, 19, 13));
    expect(parseQuickAdd("Review 11-12pm", NOW).start).toEqual(at(9, 19, 11));
    expect(parseQuickAdd("Lunch 12-1pm", NOW).start).toEqual(at(9, 19, 12));
  });

  it("keeps names that contain weekday words in the title", () => {
    expect(parseQuickAdd("Dinner with Monica 7pm", NOW).title).toBe("Dinner with Monica");
  });
});
