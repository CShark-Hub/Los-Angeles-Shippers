namespace Ember.Reports;

public sealed class ReportScheduleRunner
{
    private readonly TimeProvider scheduleClock;
    private readonly int quarterlyBatchWindow;

    public ReportScheduleRunner(TimeProvider scheduleClock, int quarterlyBatchWindow)
    {
        this.scheduleClock = scheduleClock;
        this.quarterlyBatchWindow = quarterlyBatchWindow;
    }

    public bool IsDueToday(DateOnly lastQuarterEnd)
    {
        var today = DateOnly.FromDateTime(scheduleClock.GetUtcNow().UtcDateTime);
        return today.DayNumber - lastQuarterEnd.DayNumber <= quarterlyBatchWindow;
    }
}
