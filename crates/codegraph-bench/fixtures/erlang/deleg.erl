-module(deleg).
-export([header/2, header/3, binary_arg/1]).

-spec header(binary(), map()) -> any().
header(Name, Req) ->
    header(Name, Req, undefined).

-spec header(binary(), map(), any()) -> any().
header(Name, Headers, Default) ->
    maps:get(Name, Headers, Default).

binary_arg(<<Name, Value>>) ->
    header(Name, #{value => Value}).
