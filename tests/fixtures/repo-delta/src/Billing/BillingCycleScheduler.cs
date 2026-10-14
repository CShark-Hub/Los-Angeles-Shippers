namespace Delta.Billing;

public sealed class BillingCycleScheduler
{
    private readonly BillingCalendar billingCalendar;
    private readonly LateFeeCalculator lateFeeCalculator;

    public BillingCycleScheduler(BillingCalendar billingCalendar, LateFeeCalculator lateFeeCalculator)
    {
        this.billingCalendar = billingCalendar;
        this.lateFeeCalculator = lateFeeCalculator;
    }

    public IEnumerable<BillingRunSlot> PlanUpcomingRuns(DateOnly fromDate, int cycleCount)
    {
        // FIXME: cycles that start on the 31st drift by a day in short months
        var cycleStart = billingCalendar.AlignToCycleStart(fromDate);
        for (var cycleIndex = 0; cycleIndex < cycleCount; cycleIndex++)
        {
            yield return new BillingRunSlot(cycleStart, lateFeeCalculator.GraceDaysFor(cycleStart));
            cycleStart = billingCalendar.NextCycleStart(cycleStart);
        }
    }
}

public sealed record BillingRunSlot(DateOnly RunsOn, int GraceDays);
