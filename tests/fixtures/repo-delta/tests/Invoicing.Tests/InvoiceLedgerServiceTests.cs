using Delta.Invoices;
using Xunit;

namespace Delta.Invoicing.Tests;

public sealed class InvoiceLedgerServiceTests
{
    private readonly InMemoryLedgerStore ledgerStore = new();

    [Fact]
    public void FilterByStatus_returns_only_the_requested_statuses()
    {
        ledgerStore.Seed(LedgerEntryStatus.Issued, LedgerEntryStatus.Settled, LedgerEntryStatus.Draft);
        var ledgerService = new InvoiceLedgerService(ledgerStore, new InvoiceNumberSequencer(new FixedCheckpointStore(0)));

        var settledOnly = ledgerService.FilterByStatus(FilterFor(LedgerEntryStatus.Settled));

        Assert.All(settledOnly, entry => Assert.Equal(LedgerEntryStatus.Settled, entry.Status));
    }

    [Fact]
    public void FilterByStatus_orders_the_newest_invoice_first()
    {
        ledgerStore.SeedIssuedOn(new DateOnly(2026, 9, 1), new DateOnly(2026, 9, 20));
        var ledgerService = new InvoiceLedgerService(ledgerStore, new InvoiceNumberSequencer(new FixedCheckpointStore(0)));

        var newestFirst = ledgerService.FilterByStatus(FilterFor(LedgerEntryStatus.Issued));

        Assert.Equal(new DateOnly(2026, 9, 20), newestFirst[0].IssuedOn);
    }

    [Fact]
    public void PostDraftInvoice_assigns_the_next_invoice_number()
    {
        var ledgerService = new InvoiceLedgerService(ledgerStore, new InvoiceNumberSequencer(new FixedCheckpointStore(41)));

        var postedEntry = ledgerService.PostDraftInvoice(DraftInvoice.For(new DateOnly(2026, 9, 24)));

        Assert.Equal("DLT-202609-000042", postedEntry.InvoiceNumber);
    }

    [Theory]
    [InlineData(1)]
    [InlineData(50)]
    [InlineData(200)]
    public void FilterByStatus_never_returns_more_than_a_page(int pageSize)
    {
        ledgerStore.SeedMany(LedgerEntryStatus.Issued, count: 120);
        var ledgerService = new InvoiceLedgerService(ledgerStore, new InvoiceNumberSequencer(new FixedCheckpointStore(0)));

        var onePage = ledgerService.FilterByStatus(FilterFor(LedgerEntryStatus.Issued) with { PageSize = pageSize });

        Assert.True(onePage.Count <= pageSize);
    }

    private static InvoiceQueryFilter FilterFor(LedgerEntryStatus status) =>
        new(new DateOnly(2026, 1, 1), new DateOnly(2026, 12, 31), new HashSet<LedgerEntryStatus> { status });
}
