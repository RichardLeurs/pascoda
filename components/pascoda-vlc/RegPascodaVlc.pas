unit RegPascodaVlc;

{$mode objfpc}{$H+}

// Puts the VLC components on the palette, on the VLC page.

interface

procedure Register;

implementation

uses
  Classes, PascodaVlc;

procedure Register;
begin
  RegisterComponents('VLC', [TPascodaVlcPlayer, TPascodaVlcListPlayer]);
end;

end.
