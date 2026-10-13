namespace Delta.Export;

public sealed class CsvStatementFormatter
{
    private const char ColumnSeparator = ';';

    public async Task StreamStatementRowsAsync(TextWriter output, IAsyncEnumerable<StatementLine> statementLines)
    {
        await output.WriteLineAsync("issued_on;invoice_number;amount_due;amount_paid");
        await foreach (var statementLine in statementLines)
        {
            // FIXME: amounts are formatted with the server culture instead of the account's locale
            await output.WriteLineAsync(string.Join(ColumnSeparator, statementLine.IssuedOn, statementLine.InvoiceNumber,
                statementLine.AmountDue, statementLine.AmountPaid));
        }
    }
}
