-module(store).
-export([get/1, get/2]).

get(Key) ->
    get(Key, undefined).

get(Key, Default) ->
    {Key, Default}.
