namespace Delta.Invoices;

public sealed record InvoiceQueryFilter(
    DateOnly PeriodStart,
    DateOnly PeriodEnd,
    IReadOnlySet<LedgerEntryStatus> Statuses,
    int PageSize = 50);

public enum LedgerEntryStatus
{
    Draft,
    Issued,
    PartiallyPaid,
    Settled,
    WrittenOff,
}
