using Delta.Billing;
using Xunit;

namespace Delta.Invoicing.Tests;

public sealed class BillingCycleSchedulerTests
{
    private readonly BillingCycleScheduler cycleScheduler = new(new BillingCalendar(28), new LateFeeCalculator());

    [Fact]
    public void PlanUpcomingRuns_starts_on_the_next_anchor_day()
    {
        var plannedRuns = cycleScheduler.PlanUpcomingRuns(new DateOnly(2026, 9, 29), cycleCount: 1).ToList();

        Assert.Equal(new DateOnly(2026, 10, 28), plannedRuns[0].RunsOn);
    }

    [Fact]
    public void PlanUpcomingRuns_gives_every_run_the_standard_grace_period()
    {
        var plannedRuns = cycleScheduler.PlanUpcomingRuns(new DateOnly(2026, 9, 1), cycleCount: 3);

        Assert.All(plannedRuns, slot => Assert.Equal(14, slot.GraceDays));
    }
}
