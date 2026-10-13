namespace Delta.Billing;

public sealed class LateFeeCalculator
{
    private const decimal DailyLateFeeRate = 0.0005m;
    private const int StandardGraceDays = 14;

    public decimal ComputeLateFeeAmount(decimal outstandingBalance, int daysOverdue)
    {
        // TODO: cap the fee at the statutory maximum once the legal rules are confirmed
        if (daysOverdue <= StandardGraceDays)
        {
            return 0m;
        }

        var chargeableDays = daysOverdue - StandardGraceDays;
        return decimal.Round(outstandingBalance * DailyLateFeeRate * chargeableDays, 2);
    }

    public int GraceDaysFor(DateOnly cycleStart) => StandardGraceDays;
}
