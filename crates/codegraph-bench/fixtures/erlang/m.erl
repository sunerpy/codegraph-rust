-module(m).
-export([f/1, g/0]).

-include("foo.hrl").

-define(X, 1).

-record(state, {a, b}).

-spec f(integer()) -> integer().
f(0) -> 0;
f(N) -> f(N - 1).

g() ->
    _F = fun f/1,
    _S = #state{a = ?X, b = 2},
    other:h(),
    g().
