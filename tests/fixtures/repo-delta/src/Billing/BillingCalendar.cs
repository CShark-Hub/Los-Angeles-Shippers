namespace Delta.Billing;

public sealed class BillingCalendar
{
    private readonly int cycleAnchorDay;

    public BillingCalendar(int cycleAnchorDay) => this.cycleAnchorDay = cycleAnchorDay;

    public DateOnly AlignToCycleStart(DateOnly anyDate)
    {
        var anchorInMonth = Math.Min(cycleAnchorDay, DateTime.DaysInMonth(anyDate.Year, anyDate.Month));
        var candidateStart = new DateOnly(anyDate.Year, anyDate.Month, anchorInMonth);
        return candidateStart >= anyDate ? candidateStart : NextCycleStart(candidateStart);
    }

    public DateOnly NextCycleStart(DateOnly currentStart)
    {
        var followingMonth = currentStart.AddMonths(1);
        var anchorInMonth = Math.Min(cycleAnchorDay, DateTime.DaysInMonth(followingMonth.Year, followingMonth.Month));
        return new DateOnly(followingMonth.Year, followingMonth.Month, anchorInMonth);
    }
}
