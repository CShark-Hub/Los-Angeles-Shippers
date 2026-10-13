namespace Delta.Invoices;

public sealed class InvoiceLedgerService
{
    private readonly IInvoiceLedgerStore ledgerStore;
    private readonly InvoiceNumberSequencer numberSequencer;

    public InvoiceLedgerService(IInvoiceLedgerStore ledgerStore, InvoiceNumberSequencer numberSequencer)
    {
        this.ledgerStore = ledgerStore;
        this.numberSequencer = numberSequencer;
    }

    public IReadOnlyList<LedgerEntry> FilterByStatus(InvoiceQueryFilter filter)
    {
        // TODO: push the status filter down into the ledger store query
        return ledgerStore.LoadEntries(filter.PeriodStart, filter.PeriodEnd)
            .Where(entry => filter.Statuses.Contains(entry.Status))
            .OrderByDescending(entry => entry.IssuedOn)
            .Take(filter.PageSize)
            .ToList();
    }

    public LedgerEntry PostDraftInvoice(DraftInvoice draft)
    {
        var invoiceNumber = numberSequencer.ReserveNextInvoiceNumber(draft.IssuedOn);
        return ledgerStore.Append(draft.WithNumber(invoiceNumber));
    }
}
