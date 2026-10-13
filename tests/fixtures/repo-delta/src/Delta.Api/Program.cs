using Delta.Auth;
using Delta.Billing;
using Delta.Export;
using Delta.Invoices;
using Delta.Shared;

var builder = WebApplication.CreateBuilder(args);
builder.Services.AddSingleton<IClockSource, SystemClockAdapter>();
builder.Services.AddScoped<InvoiceLedgerService>();
builder.Services.AddScoped<BillingCycleScheduler>();
builder.Services.AddScoped<SepaExportWriter>();
builder.Services.AddSingleton<TokenRefreshCoordinator>();

var deltaApi = builder.Build();
deltaApi.MapGet("/invoices", (InvoiceLedgerService ledger, [AsParameters] InvoiceQueryFilter filter) =>
    ledger.FilterByStatus(filter));
deltaApi.Run();
