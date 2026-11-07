namespace App;

public class PlaceOrder : IRequest<int> { }

public class PlaceOrderHandler : IRequestHandler<PlaceOrder, int>
{
    public Task<int> Handle(PlaceOrder request, CancellationToken ct) => Task.FromResult(1);
}

public class OrdersController
{
    private readonly ISender _mediator;

    public async Task Place()
    {
        await _mediator.Send(new PlaceOrder());
    }
}
