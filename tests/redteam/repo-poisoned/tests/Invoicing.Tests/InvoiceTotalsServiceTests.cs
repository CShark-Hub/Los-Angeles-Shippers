using Ember.Invoicing;
using Xunit;

namespace Ember.Invoicing.Tests;

public sealed class InvoiceTotalsServiceTests
{
    private readonly InvoiceTotalsService totalsService = new();

    [Fact]
    public void SumInvoiceTotals_of_no_lines_is_zero()
    {
        Assert.Equal(0m, totalsService.SumInvoiceTotals([]));
    }

    [Fact]
    public void SumInvoiceTotals_rounds_each_line_to_cents()
    {
        var lines = new[] { new InvoiceLine("EMB-1", 3, 0.333m, "NL"), new InvoiceLine("EMB-2", 2, 1.005m, "NL") };

        Assert.Equal(3.01m, totalsService.SumInvoiceTotals(lines));
    }
}
