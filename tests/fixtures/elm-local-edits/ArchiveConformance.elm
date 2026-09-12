port module ArchiveConformance exposing (main)

import Db.Archive.Edit.ArchiveEntry as Entry
import Db.Database as Database
import Db.Id
import Json.Encode as E
import Pyre
import Pyre.LocalEdits as LocalEdits


port effectOut : E.Value -> Cmd msg


port incoming : (E.Value -> msg) -> Sub msg


port observed : E.Value -> Cmd msg


submission =
    Pyre.submit (Database.fromString "archive")
        (Entry.update (Db.Id.uuid "00000000-0000-4000-8000-000000000001") [ Entry.setTitle "archive elm" ])
        (Pyre.init "archive-conformance")


main : Program () Pyre.Model E.Value
main =
    Platform.worker
        { init =
            \_ ->
                let
                    ( model, effect, _ ) =
                        submission
                in
                ( model
                , case effect of
                    Pyre.Send value ->
                        effectOut value

                    _ ->
                        Cmd.none
                )
        , update =
            \value model ->
                let
                    ( next, _ ) =
                        Pyre.update (Pyre.decodeIncomingDelta value) model

                    ( _, _, receipt ) =
                        submission
                in
                ( next
                , case Pyre.outcome receipt next of
                    Just (LocalEdits.Confirmed result) ->
                        observed (Db.Id.encodeUuid result.id)

                    _ ->
                        Cmd.none
                )
        , subscriptions = \_ -> incoming identity
        }
