{ lib }:
let
  answer = 42;
in
{
  double = x: x * 2;
  const = answer;
}
